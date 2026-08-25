use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Locale {
    #[serde(rename = "en")]
    En,
    #[serde(rename = "zh_CN")]
    ZhCn,
}

impl<'de> Deserialize<'de> for Locale {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct LocaleVisitor;

        impl<'de> serde::de::Visitor<'de> for LocaleVisitor {
            type Value = Locale;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a locale string like 'en', 'zh_CN', 'zh-CN', 'zh', etc.")
            }

            fn visit_str<E>(self, value: &str) -> Result<Locale, E>
            where
                E: serde::de::Error,
            {
                Locale::from_str(value).map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_str(LocaleVisitor)
    }
}

impl Default for Locale {
    fn default() -> Self {
        Locale::En
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Locale::En => write!(f, "en"),
            Locale::ZhCn => write!(f, "zh_CN"),
        }
    }
}

impl FromStr for Locale {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "en" | "english" | "en_us" | "en_gb" | "en_ca" | "en_au" => Ok(Locale::En),
            "zh" | "zh_cn" | "zh_hans" | "chinese" | "简体中文" | "zh_tw" | "zh_hk" | "zh_hant"
            | "繁體中文" => Ok(Locale::ZhCn),
            other => {
                if other.starts_with("en") {
                    Ok(Locale::En)
                } else if other.starts_with("zh") || other.contains("中文") {
                    Ok(Locale::ZhCn)
                } else {
                    Err(format!("unsupported locale: {s}"))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_round_trips_through_from_str() {
        assert_eq!(
            Locale::En.to_string().parse::<Locale>().unwrap(),
            Locale::En
        );
        assert_eq!(
            Locale::ZhCn.to_string().parse::<Locale>().unwrap(),
            Locale::ZhCn
        );
    }

    #[test]
    fn from_str_accepts_common_aliases() {
        assert_eq!("en".parse::<Locale>().unwrap(), Locale::En);
        assert_eq!("English".parse::<Locale>().unwrap(), Locale::En);
        assert_eq!("zh".parse::<Locale>().unwrap(), Locale::ZhCn);
        assert_eq!("zh_CN".parse::<Locale>().unwrap(), Locale::ZhCn);
        assert_eq!("zh-cn".parse::<Locale>().unwrap(), Locale::ZhCn);
        assert_eq!("简体中文".parse::<Locale>().unwrap(), Locale::ZhCn);
        // zh_TW / zh_HK fall back to ZhCn (no separate Traditional variant yet)
        assert_eq!("zh_TW".parse::<Locale>().unwrap(), Locale::ZhCn);
        assert_eq!("zh-tw".parse::<Locale>().unwrap(), Locale::ZhCn);
        assert_eq!("zh_HK".parse::<Locale>().unwrap(), Locale::ZhCn);
        assert_eq!("zh-hk".parse::<Locale>().unwrap(), Locale::ZhCn);
        assert_eq!("繁體中文".parse::<Locale>().unwrap(), Locale::ZhCn);
    }

    #[test]
    fn from_str_rejects_unknown() {
        assert!("fr".parse::<Locale>().is_err());
        assert!("".parse::<Locale>().is_err());
    }

    #[test]
    fn serde_uses_short_keys() {
        let s = serde_json::to_string(&Locale::ZhCn).unwrap();
        assert_eq!(s, r#""zh_CN""#);
        let parsed: Locale = serde_json::from_str(r#""en""#).unwrap();
        assert_eq!(parsed, Locale::En);
        let parsed_hyphen: Locale = serde_json::from_str(r#""zh-CN""#).unwrap();
        assert_eq!(parsed_hyphen, Locale::ZhCn);
        let toml_parsed: toml::Value = toml::from_str(r#"language = "zh-CN""#).unwrap();
        let lang: Option<Locale> = toml_parsed.get("language").unwrap().clone().try_into().unwrap();
        assert_eq!(lang, Some(Locale::ZhCn));

        for val in &["zh-CN", "zh_CN", "zh-cn", "zh_cn", "ZH-CN", "ZH_CN", "zh", "ZH", "简体中文", "zh-TW", "zh_TW"] {
            let toml_doc: toml::Value = toml::from_str(&format!(r#"language = "{val}""#)).unwrap();
            let parsed_lang: Option<Locale> = toml_doc.get("language").unwrap().clone().try_into().unwrap();
            assert_eq!(parsed_lang, Some(Locale::ZhCn), "failed for {val}");
        }

        for val in &["en", "EN", "en-US", "en_US", "en-us", "en_us", "English", "english"] {
            let toml_doc: toml::Value = toml::from_str(&format!(r#"language = "{val}""#)).unwrap();
            let parsed_lang: Option<Locale> = toml_doc.get("language").unwrap().clone().try_into().unwrap();
            assert_eq!(parsed_lang, Some(Locale::En), "failed for {val}");
        }
    }
}
