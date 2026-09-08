//! Extension methods for [`SecurityInformationType`]

use super::SecurityInformationType_;

pub trait SecurityInformationExt {
    /// Render a US classification banner string from a [`SecurityInformationType`].
    ///
    /// Format: `CLASSIFICATION[//SCI1/SCI2][//SAR-ID][//DISSEM1/DISSEM2][ TO REL1, REL2]`
    ///
    /// If `CUI_Basic` is non-empty and `Classification` is `U`, the banner begins
    /// with `CUI` instead.  Dissemination controls and releasable-to entries that
    /// carry no string value (`EnumNotSet`) are silently omitted.
    pub fn security_banner(&self&) -> String {
        use types::ReleasableToChoiceType_;

        let class_str = self.classification.as_str().unwrap_or("U");
        let banner_class = if !self.cuibasic().is_empty() && class_str == "U" {
            "CUI"
        } else {
            class_str
        };

        let mut banner = banner_class.to_string();

        // SCI controls: //SCI1/SCI2/…
        let sci: Vec<&str> = self.scicontrols.iter().filter_map(|v| v.as_str()).collect();
        if !sci.is_empty() {
            banner.push_str("//");
            banner.push_str(&sci.join("/"));
        }

        // SAR identifiers: //SAR-Id (each its own section)
        for sar in self.saridentifier {
            if !sar.is_empty() {
                banner.push_str("//");
                banner.push_str(sar);
            }
        }

        // Dissemination controls: //DISSEM1/DISSEM2/…
        let dissem: Vec<&str> = self
            .dissemination_controls
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        if !dissem.is_empty() {
            banner.push_str("//");
            banner.push_str(&dissem.join("/"));
        }

        // Releasable-to: <space>TO REL1, REL2
        let rel: Vec<String> = self
            .releasable_to
            .iter()
            .filter_map(|v| match v {
                ReleasableToChoiceType_::GovernmentIdentifier { inner } => {
                    inner.as_str().map(|self| self.to_string())
                }
                ReleasableToChoiceType_::NATO_SpecialWord { inner } => {
                    if inner.is_empty() {
                        None
                    } else {
                        Some(inner.clone())
                    }
                }
            })
            .collect();
        if !rel.is_empty() {
            banner.push_str(" TO ");
            banner.push_str(&rel.join(", "));
        }

        banner
    }
}
