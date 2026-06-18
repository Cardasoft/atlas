//! Authentification & autorisation du WebSocket (doc 40 §4-5) — logique pure, testée d'abord.
//! - `Channel::parse` : reconnaît le type de canal.
//! - `Pdp` : décide si une identité peut s'abonner à un canal (RBAC/ABAC, doc 38).
//! - `Authenticator` : valide le jeton à l'upgrade.

/// Contexte d'identité résolu depuis le jeton (M1 minimal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCtx {
    pub tenant: String,
    pub is_admin: bool,
}

/// Type de canal d'abonnement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
    Ingest,
    Asset(String),
    Notifications(String),
    Admin(String),
    Unknown(String),
}

impl Channel {
    pub fn parse(s: &str) -> Channel {
        match s.split_once(':') {
            None if s == "ingest" => Channel::Ingest,
            Some(("asset", id)) => Channel::Asset(id.to_string()),
            Some(("notifications", u)) => Channel::Notifications(u.to_string()),
            Some(("admin", x)) => Channel::Admin(x.to_string()),
            _ => Channel::Unknown(s.to_string()),
        }
    }
}

/// Point de décision d'autorisation (Policy Decision Point).
pub trait Pdp: Send + Sync {
    fn can_subscribe(&self, ctx: &AuthCtx, channel: &str) -> bool;
}

/// PDP par défaut : refuse les canaux `admin:*` aux non-admins et les canaux inconnus.
pub struct DefaultPdp;
impl Pdp for DefaultPdp {
    fn can_subscribe(&self, ctx: &AuthCtx, channel: &str) -> bool {
        match Channel::parse(channel) {
            Channel::Admin(_) => ctx.is_admin,
            Channel::Unknown(_) => false,
            // M1 : autres canaux autorisés dans le tenant. La vérification fine
            // (asset appartient au tenant) s'ajoutera avec l'accès aux droits (doc 27).
            _ => true,
        }
    }
}

/// Validation du jeton à l'upgrade.
pub trait Authenticator: Send + Sync {
    fn authenticate(&self, token: &str) -> Option<AuthCtx>;
}

/// Authentificateur de dev : accepte tout jeton non vide ; `admin` → contexte admin.
/// Sera remplacé par la vérification OIDC/clé (doc 16/38).
pub struct DevAuthenticator;
impl Authenticator for DevAuthenticator {
    fn authenticate(&self, token: &str) -> Option<AuthCtx> {
        if token.is_empty() {
            return None;
        }
        Some(AuthCtx {
            tenant: "default".into(),
            is_admin: token == "admin",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_channels() {
        assert_eq!(Channel::parse("ingest"), Channel::Ingest);
        assert_eq!(Channel::parse("asset:abc"), Channel::Asset("abc".into()));
        assert_eq!(
            Channel::parse("admin:queues"),
            Channel::Admin("queues".into())
        );
        assert_eq!(Channel::parse("weird"), Channel::Unknown("weird".into()));
    }

    #[test]
    fn pdp_blocks_admin_for_non_admin() {
        let pdp = DefaultPdp;
        let user = AuthCtx {
            tenant: "t".into(),
            is_admin: false,
        };
        let admin = AuthCtx {
            tenant: "t".into(),
            is_admin: true,
        };
        assert!(!pdp.can_subscribe(&user, "admin:queues"));
        assert!(pdp.can_subscribe(&admin, "admin:queues"));
    }

    #[test]
    fn pdp_blocks_unknown_channels() {
        let pdp = DefaultPdp;
        let user = AuthCtx {
            tenant: "t".into(),
            is_admin: false,
        };
        assert!(!pdp.can_subscribe(&user, "weird"));
        assert!(pdp.can_subscribe(&user, "asset:1"));
        assert!(pdp.can_subscribe(&user, "ingest"));
    }

    #[test]
    fn authenticator_rejects_empty_token() {
        let a = DevAuthenticator;
        assert!(a.authenticate("").is_none());
        assert_eq!(a.authenticate("admin").unwrap().is_admin, true);
        assert_eq!(a.authenticate("u123").unwrap().is_admin, false);
    }
}
