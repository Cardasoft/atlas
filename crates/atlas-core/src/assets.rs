//! Endpoint d'ingestion `POST /v1/assets` (doc 26 / doc 22).
//! Orchestration M1 : prepare (pur) → persistance (asset, search_text, embedding) →
//! l'asset est immédiatement cherchable. Disponible uniquement si PostgreSQL est branché.

use atlas_embed::Embedder;
use atlas_ingest::prepare::{prepare, IngestInput};
use atlas_search::Identity;
use axum::{
    extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Request, State},
    http::{header::CONTENT_TYPE, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

/// Taille maximale d'un upload `multipart` (AT-004). Garde-fou mémoire/DoS : un binaire
/// image de beta tient largement sous 25 Mio ; au-delà → 413 (axum `DefaultBodyLimit`).
const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

#[derive(Clone)]
pub struct AssetsState {
    pub db: atlas_db::Db,
    pub embedder: Arc<dyn Embedder>,
    pub hub: atlas_realtime::Hub,
    /// Cache de recherche partagé : purgé pour le tenant à chaque ingestion (doc 25 §6).
    pub cache: Arc<dyn atlas_search::cache::SearchCache>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAssetRequest {
    pub title: String,
    #[serde(default = "default_mime")]
    pub mime: String,
    /// Texte à indexer (description/OCR/transcription). Sert aussi à l'empreinte de contenu (M1).
    #[serde(default)]
    pub text: String,
    /// Origine IA **déclarée** par l'éditeur (« human », « ai_generated », « ai_edited »…).
    /// Prime sur la détection par marqueurs C2PA/IPTC (AI Act art. 50, transparence).
    #[serde(default)]
    pub provenance: Option<String>,
    /// Outil/modèle générateur déclaré, si connu (ex. « Firefly »).
    #[serde(default)]
    pub generator: Option<String>,
}
fn default_mime() -> String {
    "application/octet-stream".into()
}

#[derive(Debug, Serialize)]
pub struct CreateAssetResponse {
    pub id: Uuid,
    pub status: String,
    pub content_sha256: String,
    /// Provenance / transparence IA retenue pour l'asset (AI Act art. 50).
    pub provenance: atlas_types::Provenance,
    /// Libellé de transparence à afficher si le contenu relève de l'IA, sinon `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transparency_label: Option<&'static str>,
}

/// Réponse de `GET /v1/assets/{id}` — mappe le schéma OpenAPI `Asset` (AT-006).
#[derive(Debug, Serialize)]
pub struct AssetView {
    pub id: Uuid,
    pub tenant_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    pub status: String,
    pub rights_status: String,
    /// Provenance / transparence IA de l'asset (AI Act art. 50).
    pub provenance: atlas_types::Provenance,
}

pub fn routes(state: AssetsState) -> Router {
    Router::new()
        .route("/assets", post(create_asset))
        .route("/assets/:id", get(get_asset))
        // AT-004 : l'upload binaire passe par le corps de la requête → on relève le plafond
        // par défaut d'axum (2 Mio) pour accepter une vraie image, borné par MAX_UPLOAD_BYTES.
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
        .with_state(state)
}

/// `GET /v1/assets/{id}` (AT-006) — lit un asset du tenant authentifié (AT-001).
/// La RLS borne la lecture au tenant : un id d'un autre tenant est invisible → **404**
/// (pas de fuite d'existence inter-tenant). `Identity` impose déjà une auth valide (401 sinon).
async fn get_asset(
    State(st): State<AssetsState>,
    Identity(ctx): Identity,
    Path(id): Path<Uuid>,
) -> Result<Json<AssetView>, (StatusCode, Json<Value>)> {
    let asset = st
        .db
        .get_asset(ctx.tenant_id, id)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    Ok(Json(AssetView {
        id: asset.id,
        tenant_id: asset.tenant_id,
        title: asset.title,
        mime: asset.mime,
        status: asset.status,
        rights_status: asset.rights_status,
        provenance: asset.provenance,
    }))
}

fn not_found() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "type": "https://atlas.local/errors/not-found",
            "title": "Asset introuvable"
        })),
    )
}

/// Champs d'ingestion **possédés**, issus soit d'un corps JSON (M1, texte) soit d'un
/// upload `multipart/form-data` (AT-004, fichier binaire réel). Les deux chemins
/// convergent ensuite vers la même préparation (`prepare`) et la même persistance.
#[derive(Debug)]
struct IngestFields {
    title: String,
    mime: String,
    text: String,
    provenance: Option<String>,
    generator: Option<String>,
    /// Octets dont on calcule l'empreinte exacte (`content_sha256`) et la provenance C2PA.
    /// Upload : octets **réels** du fichier ; JSON : octets du texte (compat M1).
    bytes: Vec<u8>,
}

/// Vrai si l'en-tête `Content-Type` désigne un upload multipart (AT-004).
fn content_type_is_multipart(ct: &str) -> bool {
    ct.split(';')
        .next()
        .map(|s| s.trim().eq_ignore_ascii_case("multipart/form-data"))
        .unwrap_or(false)
}

/// Lit les champs d'un upload `multipart/form-data` (AT-004) : la partie `file` porte les
/// **octets binaires réels** ; les autres parties sont des champs texte (`title`, `text`,
/// `mime`, `provenance`, `generator`). MIME retenu = champ `mime` explicite, sinon le
/// `Content-Type` de la partie fichier, sinon le défaut. Titre absent → repli sur le nom
/// de fichier. Empreinte calculée sur les octets du fichier si présent, sinon sur le texte
/// (compat M1). Aucune dépendance à la base → unitairement testable.
async fn read_multipart_fields(mut mp: Multipart) -> Result<IngestFields, String> {
    let mut title: Option<String> = None;
    let mut text = String::new();
    let mut mime_field: Option<String> = None;
    let mut provenance: Option<String> = None;
    let mut generator: Option<String> = None;
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut file_mime: Option<String> = None;
    let mut file_name: Option<String> = None;

    while let Some(field) = mp.next_field().await.map_err(|e| e.to_string())? {
        let name = field.name().map(str::to_owned);
        match name.as_deref() {
            Some("file") => {
                file_mime = field.content_type().map(str::to_owned);
                file_name = field.file_name().map(str::to_owned);
                let bytes = field.bytes().await.map_err(|e| e.to_string())?;
                file_bytes = Some(bytes.to_vec());
            }
            Some("title") => title = Some(field.text().await.map_err(|e| e.to_string())?),
            Some("text") => text = field.text().await.map_err(|e| e.to_string())?,
            Some("mime") => mime_field = Some(field.text().await.map_err(|e| e.to_string())?),
            Some("provenance") => provenance = Some(field.text().await.map_err(|e| e.to_string())?),
            Some("generator") => generator = Some(field.text().await.map_err(|e| e.to_string())?),
            // Partie inconnue : on la draine pour ne pas bloquer le flux multipart.
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let title = title
        .or(file_name)
        .ok_or_else(|| "champ « title » ou fichier nommé requis".to_string())?;
    let mime = mime_field.or(file_mime).unwrap_or_else(default_mime);
    let bytes = match file_bytes {
        Some(b) => b,
        None => text.clone().into_bytes(),
    };
    Ok(IngestFields {
        title,
        mime,
        text,
        provenance,
        generator,
        bytes,
    })
}

/// `POST /v1/assets` — ingestion d'un asset (AT-001 : `Identity` impose une auth valide).
/// Deux contrats acceptés :
/// - **`multipart/form-data`** (AT-004) : upload d'un **fichier binaire réel** (partie `file`) ;
///   l'empreinte `content_sha256` porte sur les **octets du fichier** et la détection C2PA/IPTC
///   scanne ces octets (transparence IA, AI Act art. 50).
/// - **`application/json`** (compat M1) : `{title, mime, text, provenance, generator}` ;
///   l'empreinte porte sur le texte (comportement historique préservé).
///
/// Le pHash perceptuel réel (`luma_8x8` sur l'image décodée) est traité par AT-004b (décodeur
/// image dédié, décision de dépendance/MSRV séparée) → `luma_8x8 = None` ici.
async fn create_asset(
    State(st): State<AssetsState>,
    Identity(ctx): Identity,
    req: Request,
) -> Result<(StatusCode, Json<CreateAssetResponse>), (StatusCode, Json<Value>)> {
    let tenant = ctx.tenant_id;

    let is_multipart = req
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(content_type_is_multipart)
        .unwrap_or(false);

    let fields = if is_multipart {
        let mp = Multipart::from_request(req, &st)
            .await
            .map_err(|e| bad_request(&e.to_string()))?;
        read_multipart_fields(mp)
            .await
            .map_err(|m| bad_request(&m))?
    } else {
        let Json(body) = Json::<CreateAssetRequest>::from_request(req, &st)
            .await
            .map_err(|e| bad_request(&e.to_string()))?;
        IngestFields {
            bytes: body.text.clone().into_bytes(), // compat M1 : empreinte sur le texte.
            title: body.title,
            mime: body.mime,
            text: body.text,
            provenance: body.provenance,
            generator: body.generator,
        }
    };

    let input = IngestInput {
        title: &fields.title,
        mime: &fields.mime,
        text: &fields.text,
        bytes: &fields.bytes, // AT-004 : octets réels de l'upload (sinon texte, compat M1).
        luma_8x8: None,       // pHash réel sur l'image décodée : AT-004b.
        declared_provenance: fields.provenance.as_deref(),
        generator: fields.generator.as_deref(),
    };
    let prepared = prepare(&input, st.embedder.as_ref());

    let id = st
        .db
        .insert_asset(
            tenant,
            &fields.title,
            &fields.mime,
            prepared.status,
            "none",
            None,
            None,
            &prepared.provenance,
        )
        .await
        .map_err(internal)?;
    st.db
        .upsert_search_text(tenant, id, "simple", &prepared.search_text)
        .await
        .map_err(internal)?;
    st.db
        .upsert_embedding(tenant, id, "fake", &prepared.embedding)
        .await
        .map_err(internal)?;

    // Le périmètre du tenant a changé : on purge son cache de recherche pour éviter de servir
    // des résultats périmés qui ignoreraient l'asset fraîchement indexé (doc 25 §6).
    st.cache.invalidate_tenant(tenant).await;

    // Temps réel : notifie les UI abonnées (canaux "ingest" et "asset:{id}"), doc 40.
    // On transmet la provenance pour que l'UI affiche d'emblée le libellé de transparence.
    let label = prepared.provenance.transparency_label();
    let payload = json!({
        "id": id,
        "status": prepared.status,
        "title": fields.title,
        "ai_provenance": prepared.provenance.ai.as_str(),
        "c2pa_present": prepared.provenance.c2pa_present,
        "transparency_label": label,
    });
    st.hub.publish("ingest", "asset.ingested", payload.clone());
    st.hub
        .publish(format!("asset:{id}"), "asset.ingested", payload);

    Ok((
        StatusCode::CREATED,
        Json(CreateAssetResponse {
            id,
            status: prepared.status.to_string(),
            content_sha256: prepared.content_sha256,
            transparency_label: label,
            provenance: prepared.provenance,
        }),
    ))
}

fn internal(e: atlas_db::DbError) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "type": "https://atlas.local/errors/internal",
            "title": "Erreur interne",
            "detail": e.to_string()
        })),
    )
}

/// Corps mal formé (multipart illisible, JSON invalide, champ requis absent) → 400 (RFC 9457).
fn bad_request(detail: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "type": "https://atlas.local/errors/bad-request",
            "title": "Requête invalide",
            "detail": detail,
        })),
    )
}

#[cfg(test)]
mod tests {
    //! Tests **sans base** de la nouvelle logique d'upload (AT-004) : détection du type de
    //! contenu et extraction des champs multipart (dont les octets binaires réels). La
    //! correction de l'empreinte sur octets réels est vérifiée de bout en bout via `prepare`.
    use super::*;
    use atlas_embed::FakeEmbedder;
    use atlas_ingest::hash::sha256_hex;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;

    enum PartVal<'a> {
        Text(&'a str),
        File {
            filename: &'a str,
            content_type: &'a str,
            bytes: &'a [u8],
        },
    }

    /// Assemble un corps `multipart/form-data` brut (boundary fixe) + son en-tête Content-Type.
    fn build_multipart(parts: &[(&str, PartVal)]) -> (String, Vec<u8>) {
        let boundary = "ATLASBOUNDARYtest";
        let mut body: Vec<u8> = Vec::new();
        for (name, val) in parts {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            match val {
                PartVal::Text(t) => {
                    body.extend_from_slice(
                        format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n")
                            .as_bytes(),
                    );
                    body.extend_from_slice(t.as_bytes());
                }
                PartVal::File {
                    filename,
                    content_type,
                    bytes,
                } => {
                    body.extend_from_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n"
                        )
                        .as_bytes(),
                    );
                    body.extend_from_slice(
                        format!("Content-Type: {content_type}\r\n\r\n").as_bytes(),
                    );
                    body.extend_from_slice(bytes);
                }
            }
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        (format!("multipart/form-data; boundary={boundary}"), body)
    }

    async fn parse(parts: &[(&str, PartVal<'_>)]) -> Result<IngestFields, String> {
        let (ct, body) = build_multipart(parts);
        let req = HttpRequest::builder()
            .method("POST")
            .header(CONTENT_TYPE, ct)
            .body(Body::from(body))
            .unwrap();
        let mp = Multipart::from_request(req, &())
            .await
            .map_err(|e| e.to_string())?;
        read_multipart_fields(mp).await
    }

    #[test]
    fn detects_multipart_content_type() {
        assert!(content_type_is_multipart(
            "multipart/form-data; boundary=xyz"
        ));
        assert!(content_type_is_multipart(
            "Multipart/Form-Data; boundary=xyz"
        ));
        assert!(!content_type_is_multipart("application/json"));
        assert!(!content_type_is_multipart("text/plain; charset=utf-8"));
    }

    #[tokio::test]
    async fn multipart_upload_captures_real_file_bytes_and_mime() {
        // Octets « image » contenant un zéro (non-UTF-8) : prouve qu'on transporte du binaire.
        let img: &[u8] = b"\xFF\xD8\xFFreel-jpeg\x00\x01\x02bytes";
        let f = parse(&[
            ("title", PartVal::Text("Plage été")),
            (
                "file",
                PartVal::File {
                    filename: "plage.jpg",
                    content_type: "image/jpeg",
                    bytes: img,
                },
            ),
            ("provenance", PartVal::Text("ai_generated")),
        ])
        .await
        .unwrap();

        assert_eq!(f.title, "Plage été");
        assert_eq!(f.mime, "image/jpeg"); // hérité du Content-Type de la partie fichier
        assert_eq!(f.bytes, img); // octets binaires réels conservés tels quels
        assert_eq!(f.provenance.as_deref(), Some("ai_generated"));

        // AT-004 : l'empreinte porte sur les OCTETS RÉELS du fichier, pas sur le texte/titre.
        let input = IngestInput {
            title: &f.title,
            mime: &f.mime,
            text: &f.text,
            bytes: &f.bytes,
            luma_8x8: None,
            declared_provenance: f.provenance.as_deref(),
            generator: f.generator.as_deref(),
        };
        let prepared = prepare(&input, &FakeEmbedder);
        assert_eq!(prepared.content_sha256, sha256_hex(img));
        assert_ne!(prepared.content_sha256, sha256_hex(f.title.as_bytes()));
    }

    #[tokio::test]
    async fn explicit_mime_field_overrides_file_content_type() {
        let f = parse(&[
            (
                "file",
                PartVal::File {
                    filename: "a.bin",
                    content_type: "application/octet-stream",
                    bytes: b"xyz",
                },
            ),
            ("mime", PartVal::Text("image/png")),
        ])
        .await
        .unwrap();
        assert_eq!(f.mime, "image/png");
        assert_eq!(f.title, "a.bin"); // titre absent → repli sur le nom de fichier
    }

    #[tokio::test]
    async fn missing_title_and_unnamed_file_is_rejected() {
        let err = parse(&[("text", PartVal::Text("sans titre ni fichier"))])
            .await
            .unwrap_err();
        assert!(err.contains("title"), "message inattendu : {err}");
    }
}
