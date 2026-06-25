use crate::email::Email;

/// Universal confirmation data extracted from any forwarding confirmation email.
#[derive(Debug, Clone)]
pub struct Confirmation {
    pub originator: String,
    pub confirm_url: String,
}

/// Provider detection, extraction, and template association.
pub struct Provider {
    pub from_address: &'static str,
    pub template: &'static str,
    pub extract: fn(&Email) -> Option<Confirmation>,
}

impl Provider {
    pub fn detect(&self, email: &Email) -> bool {
        email.info.from_address == self.from_address
    }

    pub fn render(&self, name: &str, confirmation: &Confirmation) -> String {
        self.template
            .replace("{originator}", &confirmation.originator)
            .replace("{confirm_url}", &confirmation.confirm_url)
            .replace("{provider}", name)
    }
}

static DEFAULT_PROVIDER: Provider = Provider {
    from_address: "",
    template: include_str!("templates/confirm.txt"),
    extract: |_| None,
};

/// All known providers, keyed by name.
pub static PROVIDERS: &[(&str, Provider)] = &[
    ("Gmail", Provider {
        from_address: "forwarding-noreply@google.com",
        template: include_str!("templates/gmail.txt"),
        extract: gmail::extract,
    }),
    ("O365", Provider {
        from_address: "noreply@microsoft.com",
        template: include_str!("templates/confirm.txt"),
        extract: o365::extract,
    }),
];

/// Is this email a forwarding request from any known provider?
pub fn is_forwarding_request(email: &Email) -> bool {
    get_forwarding_provider(email).is_some()
}

/// Identify which provider sent this forwarding request.
pub fn get_forwarding_provider(email: &Email) -> Option<(&'static str, &'static Provider)> {
    PROVIDERS.iter()
        .find(|(_, provider)| provider.detect(email))
        .map(|(name, provider)| (*name, provider))
}

/// Look up a provider by name, falling back to the default.
pub fn get_provider(name: &str) -> &'static Provider {
    PROVIDERS.iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
        .unwrap_or(&DEFAULT_PROVIDER)
}

mod gmail {
    use regex::Regex;
    use super::Confirmation;
    use crate::email::Email;

    pub fn extract(email: &Email) -> Option<Confirmation> {
        let body = email.body.as_deref()?;

        let originator_re = Regex::new(r"(?m)^(\S+@\S+) has requested to automatically forward")
            .expect("invalid regex");
        let originator = originator_re
            .captures(body)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())?;

        let url_re = Regex::new(r"(https://mail(?:-settings)?\.google\.com/mail/vf-\S+)")
            .expect("invalid regex");
        let confirm_url = url_re
            .captures(body)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())?;

        Some(Confirmation {
            originator,
            confirm_url,
        })
    }
}

mod o365 {
    use regex::Regex;
    use super::Confirmation;
    use crate::email::Email;

    pub fn extract(email: &Email) -> Option<Confirmation> {
        let body = email.body.as_deref()?;

        let originator_re = Regex::new(r"(\S+@\S+).*requested.*forward")
            .expect("invalid regex");
        let originator = originator_re
            .captures(body)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())?;

        let url_re = Regex::new(r"(https://\S+)")
            .expect("invalid regex");
        let confirm_url = url_re
            .captures(body)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())?;

        Some(Confirmation {
            originator,
            confirm_url,
        })
    }
}
