//! Cache de résultats **cohérent avec les droits** (doc 25 §6).
//!
//! Clé = `tenant | requête normalisée | filtres | mode | page | auth_fingerprint`, où
//! `auth_fingerprint` résume le **contexte de permissions**. Inclure cette empreinte dans la
//! clé garantit qu'un utilisateur ne reçoit jamais le cache d'un autre périmètre (test §10.1 :
//! deux rôles ⇒ clés distinctes). TTL court (ex. 60 s) : limite le besoin d'invalidation fine.
//!
//! M1 souverain : store **en mémoire** (sans dépendance) derrière un trait ; en production le
//! store est Valkey (clé hachée → résultats sérialisés) sans changer la logique appelante.

use crate::{AuthCtx, SearchResponse};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Empreinte du contexte de permissions (doc 25 §6). M1 : tenant + utilisateur ; à terme rôle
/// + attributs + périmètre. Déterministe et stable : deux contextes identiques ⇒ même empreinte,
/// deux contextes différents ⇒ empreintes différentes (isolation du cache).
pub fn auth_fingerprint(ctx: &AuthCtx) -> String {
    let user = ctx.user_id.map(|u| u.simple().to_string()).unwrap_or_else(|| "-".into());
    format!("t={};u={}", ctx.tenant_id.simple(), user)
}

/// Clé de cache canonique et déterministe. La requête est normalisée (trim + minuscules) ;
/// les filtres sont sérialisés (ordre de champs figé par la structure) ; `page` discrimine
/// la position de pagination (curseur) ; l'empreinte de droits isole les périmètres.
#[allow(clippy::too_many_arguments)]
pub fn cache_key(
    fingerprint: &str,
    query: &str,
    mode: &str,
    example_asset_id: Option<Uuid>,
    filters_json: &str,
    page_size: usize,
    cursor: Option<&str>,
) -> String {
    let q = query.trim().to_lowercase();
    let example = example_asset_id.map(|e| e.simple().to_string()).unwrap_or_default();
    let page = cursor.unwrap_or("");
    format!("{fingerprint}|q={q}|m={mode}|ex={example}|f={filters_json}|ps={page_size}|c={page}")
}

/// Cache de réponses de recherche. Implémentations interchangeables (in-mem M1, Valkey en prod).
/// `put`/`get` portent sur des réponses **complètes** déjà hydratées. `invalidate_tenant` purge
/// le périmètre d'un tenant (appelé à l'ingestion/maj d'assets — doc 25 §6).
#[async_trait]
pub trait SearchCache: Send + Sync {
    async fn get(&self, key: &str) -> Option<SearchResponse>;
    async fn put(&self, key: String, tenant: Uuid, value: SearchResponse);
    async fn invalidate_tenant(&self, tenant: Uuid);
}

/// Cache inerte (dev/tests, ou désactivation) : toujours un miss, ne stocke rien.
pub struct NoopCache;
#[async_trait]
impl SearchCache for NoopCache {
    async fn get(&self, _key: &str) -> Option<SearchResponse> {
        None
    }
    async fn put(&self, _key: String, _tenant: Uuid, _value: SearchResponse) {}
    async fn invalidate_tenant(&self, _tenant: Uuid) {}
}

struct Entry {
    expires_at: Instant,
    tenant: Uuid,
    value: SearchResponse,
}

/// Cache en mémoire à TTL court (M1 souverain). Purge paresseuse à la lecture (entrée expirée
/// = miss + suppression). `invalidate_tenant` retire toutes les entrées du tenant.
pub struct InMemoryTtlCache {
    ttl: Duration,
    map: Mutex<HashMap<String, Entry>>,
}

impl InMemoryTtlCache {
    pub fn new(ttl: Duration) -> Self {
        Self { ttl, map: Mutex::new(HashMap::new()) }
    }
}

#[async_trait]
impl SearchCache for InMemoryTtlCache {
    async fn get(&self, key: &str) -> Option<SearchResponse> {
        let mut map = self.map.lock().unwrap();
        match map.get(key) {
            Some(e) if e.expires_at > Instant::now() => Some(e.value.clone()),
            Some(_) => {
                map.remove(key); // expirée : purge paresseuse
                None
            }
            None => None,
        }
    }

    async fn put(&self, key: String, tenant: Uuid, value: SearchResponse) {
        let entry = Entry { expires_at: Instant::now() + self.ttl, tenant, value };
        self.map.lock().unwrap().insert(key, entry);
    }

    async fn invalidate_tenant(&self, tenant: Uuid) {
        self.map.lock().unwrap().retain(|_, e| e.tenant != tenant);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tenant: u128, user: Option<u128>) -> AuthCtx {
        AuthCtx { tenant_id: Uuid::from_u128(tenant), user_id: user.map(Uuid::from_u128) }
    }

    #[test]
    fn fingerprint_differs_by_permission_context() {
        // Deux utilisateurs (rôles/périmètres) distincts → empreintes distinctes (isolation §10.1).
        let a = auth_fingerprint(&ctx(1, Some(10)));
        let b = auth_fingerprint(&ctx(1, Some(20)));
        assert_ne!(a, b);
        // Même contexte → empreinte stable.
        assert_eq!(a, auth_fingerprint(&ctx(1, Some(10))));
        // Tenant différent → empreinte différente.
        assert_ne!(a, auth_fingerprint(&ctx(2, Some(10))));
    }

    #[test]
    fn key_isolates_by_fingerprint() {
        let k1 = cache_key("t=1;u=10", "mer", "natural", None, "{}", 50, None);
        let k2 = cache_key("t=1;u=20", "mer", "natural", None, "{}", 50, None);
        assert_ne!(k1, k2, "deux périmètres ⇒ deux clés (pas de fuite de cache)");
    }

    #[test]
    fn key_normalizes_query() {
        let a = cache_key("fp", " Mer ", "natural", None, "{}", 50, None);
        let b = cache_key("fp", "mer", "natural", None, "{}", 50, None);
        assert_eq!(a, b, "trim + minuscules → même clé");
    }

    #[test]
    fn key_discriminates_page_mode_and_filters() {
        let base = cache_key("fp", "mer", "natural", None, "{}", 50, None);
        assert_ne!(base, cache_key("fp", "mer", "natural", None, "{}", 50, Some("CUR")));
        assert_ne!(base, cache_key("fp", "mer", "lexical", None, "{}", 50, None));
        assert_ne!(base, cache_key("fp", "mer", "natural", None, r#"{"orientation":"landscape"}"#, 50, None));
        assert_ne!(base, cache_key("fp", "mer", "natural", None, "{}", 10, None));
    }

    fn empty_response() -> SearchResponse {
        SearchResponse {
            results: vec![],
            interpreted_query: crate::understanding::InterpretedQuery {
                semantic_text: String::new(),
                filters: Default::default(),
                confidence: 0.0,
                editable: true,
            },
            facets: Default::default(),
            next_cursor: None,
            query_hash: "0".into(),
            degraded: false,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalidate_purges_only_target_tenant() {
        let (t1, t2) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let cache = InMemoryTtlCache::new(Duration::from_secs(60));
        cache.put("k1".into(), t1, empty_response()).await;
        cache.put("k2".into(), t2, empty_response()).await;
        cache.invalidate_tenant(t1).await;
        // Le tenant ingéré est purgé ; l'autre périmètre reste servi.
        assert!(cache.get("k1").await.is_none(), "le tenant purgé devient un miss");
        assert!(cache.get("k2").await.is_some(), "les autres tenants sont préservés");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn expired_entry_is_a_miss() {
        let cache = InMemoryTtlCache::new(Duration::from_millis(0)); // expire immédiatement
        cache.put("k".into(), Uuid::nil(), empty_response()).await;
        assert!(cache.get("k").await.is_none(), "entrée expirée → miss (purge paresseuse)");
    }
}
