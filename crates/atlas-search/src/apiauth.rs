//! Authentification de périmètre de l'API `/v1` (AT-001, doc 38).
//!
//! Remplace les **en-têtes de confiance** (`x-atlas-tenant`/`x-atlas-user`, stand-in M1) par une
//! authentification **non falsifiable** par **clé d'API** (`Authorization: Bearer <clé>`). En
//! production, l'identité d'un appelant ne provient QUE d'une clé valide : une clé absente,
//! vide ou inconnue → 401. En dev/air-gap (aucune clé configurée), on conserve le comportement
//! par en-têtes (mono-tenant local), explicitement signalé au démarrage.
//!
//! Souveraineté / faisabilité solo : clés **statiques** provisionnées par configuration
//! (`ATLAS_API_KEYS`), **hachées au repos** (SHA-256, jamais stockées/loguées en clair),
//! air-gap compatible (aucun IdP externe). Le trait [`ApiAuthenticator`] permet de brancher
//! plus tard un store en base (RBAC, édition Enterprise) ou OIDC **sans changer les appelants**.

use crate::AuthCtx;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use uuid::Uuid;

/// Identifiants présentés par une requête entrante. Le type reste **agnostique du transport**
/// (extrait des en-têtes HTTP par l'appelant) pour être testable sans serveur.
#[derive(Debug, Clone, Default)]
pub struct Credentials {
    /// Jeton porteur (`Authorization: Bearer <token>`), le cas échéant.
    pub bearer: Option<String>,
    /// En-têtes de dev (`x-atlas-tenant` / `x-atlas-user`) — utilisés UNIQUEMENT en mode dev.
    pub dev_tenant: Option<String>,
    pub dev_user: Option<String>,
}

/// Échec d'authentification → 401 (l'appelant ne prouve pas une identité valide).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthError;

/// Vérifie les identifiants d'une requête et résout un `AuthCtx` **non falsifiable**.
pub trait ApiAuthenticator: Send + Sync {
    /// Résout l'identité ou échoue (401). Aucune information de timing ne doit dépendre du
    /// secret en clair : les implémentations comparent des **hachages**, pas la clé brute.
    fn authenticate(&self, creds: &Credentials) -> Result<AuthCtx, AuthError>;

    /// `true` si cet authentificateur applique une **vraie** auth (clé requise). Sert au log
    /// de démarrage et aux tests ; la sécurité ne dépend **pas** de ce drapeau.
    fn enforces(&self) -> bool;
}

/// Hash SHA-256 hexadécimal d'une clé d'API (jamais stockée/loguée en clair).
fn hash_key(key: &str) -> String {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    let digest = h.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// Authentificateur par **clés statiques** (provisionnées en configuration). Devient le mode
/// par défaut dès qu'au moins une clé est configurée. Une clé absente/inconnue → 401.
pub struct StaticKeyAuthenticator {
    /// hash SHA-256 (hex) de la clé → identité résolue (non falsifiable).
    keys: HashMap<String, AuthCtx>,
}

impl StaticKeyAuthenticator {
    pub fn new(keys: HashMap<String, AuthCtx>) -> Self {
        Self { keys }
    }

    /// Nombre de clés effectivement chargées.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

impl ApiAuthenticator for StaticKeyAuthenticator {
    fn authenticate(&self, creds: &Credentials) -> Result<AuthCtx, AuthError> {
        // Les en-têtes de confiance sont **ignorés** ici : seule une clé valide donne une
        // identité (non falsifiable). Pas de jeton / jeton vide → 401.
        let token = creds.bearer.as_deref().map(str::trim).unwrap_or("");
        if token.is_empty() {
            return Err(AuthError);
        }
        self.keys.get(&hash_key(token)).cloned().ok_or(AuthError)
    }

    fn enforces(&self) -> bool {
        true
    }
}

/// Authentificateur de **dev / air-gap** : conserve le comportement par en-têtes de confiance
/// (mono-tenant local). **À NE JAMAIS utiliser en production** (identité falsifiable). N'échoue
/// jamais : défaut mono-tenant (tenant nil) quand aucun en-tête n'est fourni.
pub struct DevHeaderAuthenticator;

impl ApiAuthenticator for DevHeaderAuthenticator {
    fn authenticate(&self, creds: &Credentials) -> Result<AuthCtx, AuthError> {
        Ok(crate::resolve_auth(
            creds.dev_tenant.as_deref(),
            creds.dev_user.as_deref(),
        ))
    }

    fn enforces(&self) -> bool {
        false
    }
}

/// Parse la variable `ATLAS_API_KEYS` : entrées séparées par des virgules ou des retours à la
/// ligne, chacune `clé:tenant_uuid[:user_uuid]`. Les entrées invalides (clé vide, UUID de tenant
/// illisible) sont **ignorées** (philosophie « warn & fall back » du projet) ; le store renvoyé
/// ne contient que les clés valides, hachées. Renvoie aussi le nombre d'entrées **rejetées**
/// (pour un log de démarrage côté appelant, sans imprimer la moindre clé).
pub fn parse_api_keys(raw: &str) -> (HashMap<String, AuthCtx>, usize) {
    let mut store = HashMap::new();
    let mut rejected = 0usize;
    for entry in raw.split([',', '\n']) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let mut parts = entry.splitn(3, ':');
        let key = parts.next().unwrap_or("").trim();
        let tenant = parts.next().unwrap_or("").trim();
        let user = parts.next().map(str::trim).filter(|s| !s.is_empty());

        let tenant_id = match Uuid::parse_str(tenant) {
            Ok(id) => id,
            Err(_) => {
                rejected += 1;
                continue;
            }
        };
        if key.is_empty() {
            rejected += 1;
            continue;
        }
        let user_id = match user {
            Some(u) => match Uuid::parse_str(u) {
                Ok(id) => Some(id),
                Err(_) => {
                    rejected += 1;
                    continue;
                }
            },
            None => None,
        };
        store.insert(hash_key(key), AuthCtx { tenant_id, user_id });
    }
    (store, rejected)
}

/// Construit l'authentificateur de l'API à partir de la configuration des clés.
///
/// - `None` / chaîne vide → [`DevHeaderAuthenticator`] (dev/air-gap, identité par en-têtes).
/// - Sinon → [`StaticKeyAuthenticator`] (clé requise ; 401 sans clé valide).
///
/// **Fail-closed** : si `ATLAS_API_KEYS` est défini mais que toutes les entrées sont invalides,
/// on ne retombe **pas** silencieusement en dev (ce serait ouvrir l'accès) — on renvoie un store
/// vide qui **refuse tout** (401). Le `usize` renvoyé est le nombre de clés chargées (log).
pub fn build_authenticator(api_keys: Option<&str>) -> (Arc<dyn ApiAuthenticator>, usize) {
    match api_keys.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => {
            let (store, _rejected) = parse_api_keys(raw);
            let n = store.len();
            (Arc::new(StaticKeyAuthenticator::new(store)), n)
        }
        None => (Arc::new(DevHeaderAuthenticator), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(s: &str) -> Uuid {
        Uuid::parse_str(s).unwrap()
    }

    #[test]
    fn hash_key_is_deterministic_and_not_plaintext() {
        let h = hash_key("s3cr3t-key");
        assert_eq!(h, hash_key("s3cr3t-key")); // déterministe
        assert_eq!(h.len(), 64); // SHA-256 hex
        assert!(!h.contains("s3cr3t")); // jamais la clé en clair
        assert_ne!(hash_key("a"), hash_key("b"));
    }

    #[test]
    fn dev_authenticator_resolves_headers_and_never_fails() {
        let a = DevHeaderAuthenticator;
        assert!(!a.enforces());
        let t = uuid("11111111-1111-1111-1111-111111111111");
        let ctx = a
            .authenticate(&Credentials {
                bearer: None,
                dev_tenant: Some(t.to_string()),
                dev_user: None,
            })
            .unwrap();
        assert_eq!(ctx.tenant_id, t);
        // Absence d'en-tête → tenant nil (mono-tenant dev), jamais d'erreur.
        let ctx = a.authenticate(&Credentials::default()).unwrap();
        assert_eq!(ctx.tenant_id, Uuid::nil());
    }

    #[test]
    fn static_authenticator_requires_a_valid_key() {
        let t = uuid("22222222-2222-2222-2222-222222222222");
        let u = uuid("33333333-3333-3333-3333-333333333333");
        let (store, rejected) = parse_api_keys(&format!("clef-prod:{t}:{u}"));
        assert_eq!(rejected, 0);
        let a = StaticKeyAuthenticator::new(store);
        assert!(a.enforces());
        assert_eq!(a.len(), 1);

        // Clé valide → identité exacte (tenant + user), non falsifiable.
        let ok = a
            .authenticate(&Credentials {
                bearer: Some("clef-prod".into()),
                // Les en-têtes de confiance ne doivent RIEN changer en mode clé.
                dev_tenant: Some(Uuid::nil().to_string()),
                dev_user: None,
            })
            .unwrap();
        assert_eq!(ok.tenant_id, t);
        assert_eq!(ok.user_id, Some(u));

        // Mauvaise clé, clé vide, pas de jeton → 401.
        for bad in [Some("mauvaise".to_string()), Some(String::new()), None] {
            assert_eq!(
                a.authenticate(&Credentials {
                    bearer: bad,
                    dev_tenant: Some(t.to_string()), // ne sauve pas : en-têtes ignorés
                    dev_user: None,
                }),
                Err(AuthError)
            );
        }
    }

    #[test]
    fn parse_skips_invalid_entries_and_keeps_valid_ones() {
        let t = uuid("44444444-4444-4444-4444-444444444444");
        let raw = format!(
            "bonne:{t}\n  \n,clef-sans-tenant:pas-un-uuid,:{t},   ,valide2:{t}:also-bad-user"
        );
        let (store, rejected) = parse_api_keys(&raw);
        // « bonne:{t} » valide ; les 3 autres entrées non vides invalides (tenant illisible,
        // clé vide, user illisible) → rejetées.
        assert_eq!(store.len(), 1);
        assert_eq!(rejected, 3);
    }

    #[test]
    fn build_authenticator_modes() {
        // Aucune clé → dev (n'applique pas la clé).
        let (dev, n) = build_authenticator(None);
        assert!(!dev.enforces());
        assert_eq!(n, 0);
        let (dev2, _) = build_authenticator(Some("   "));
        assert!(!dev2.enforces());

        // Au moins une clé valide → enforce.
        let t = uuid("55555555-5555-5555-5555-555555555555");
        let (prod, n) = build_authenticator(Some(&format!("k:{t}")));
        assert!(prod.enforces());
        assert_eq!(n, 1);

        // Clés définies mais TOUTES invalides → fail-closed : enforce + refuse tout.
        let (closed, n) = build_authenticator(Some("k:pas-un-uuid"));
        assert!(closed.enforces());
        assert_eq!(n, 0);
        assert_eq!(
            closed.authenticate(&Credentials {
                bearer: Some("k".into()),
                ..Default::default()
            }),
            Err(AuthError)
        );
    }
}
