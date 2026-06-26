//! Configuration d'Atlas (M0). Tout est local ; aucune dépendance externe runtime.
//! L'IA est locale par défaut ; toute sortie externe est opt-in (doc 02/16).

use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: String,
    pub database_url: String,
    pub nats_url: String,
    /// false par défaut : aucune API LLM externe. Activable explicitement (doc 16 F54).
    pub allow_external_llm: bool,
    pub edition: Edition,
    /// Répertoire du front WASM bâti (trunk `dist/`) servi en statique par le Core.
    /// `None` → API seule (le front est servi par `trunk serve` en dev). Beta web :
    /// pointer sur le `dist/` pour qu'un binaire unique serve l'UI **et** l'API.
    pub web_dir: Option<String>,
    /// Clés d'API de périmètre (AT-001), brutes : `clé:tenant_uuid[:user_uuid]` séparées par
    /// des virgules / retours ligne. `None`/vide → mode **dev** (en-têtes de confiance,
    /// identité falsifiable) ; sinon auth par clé **requise** (identité non falsifiable). Le
    /// secret n'est jamais loggé ; le parsing/hachage vit dans `atlas_search::apiauth`.
    pub api_keys: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    Solo,
    Team,
    Enterprise,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("variable manquante: {0}")]
    Missing(&'static str),
}

impl Config {
    /// Charge depuis l'environnement avec des défauts « frugaux » (profil Solo).
    pub fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            bind_addr: env::var("ATLAS_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            database_url: env::var("ATLAS_DATABASE_URL")
                .unwrap_or_else(|_| "postgres://atlas:atlas@localhost:5432/atlas".into()),
            nats_url: env::var("ATLAS_NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".into()),
            allow_external_llm: env::var("ATLAS_ALLOW_EXTERNAL_LLM")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            edition: match env::var("ATLAS_EDITION").as_deref() {
                Ok("team") => Edition::Team,
                Ok("enterprise") => Edition::Enterprise,
                _ => Edition::Solo,
            },
            // Servir le front statique uniquement si la variable est définie (vide → ignorée).
            web_dir: env::var("ATLAS_WEB_DIR").ok().filter(|s| !s.is_empty()),
            // Clés d'API de périmètre : définies → auth par clé ; absentes/vides → dev.
            api_keys: env::var("ATLAS_API_KEYS")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sovereign() {
        // Sans variables, l'IA externe est désactivée (souveraineté par défaut).
        let c = Config::from_env().unwrap();
        assert!(!c.allow_external_llm);
    }
}
