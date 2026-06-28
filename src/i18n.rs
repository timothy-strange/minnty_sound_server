use serde_json::Value;
use std::sync::OnceLock;

static EN_STRINGS: &str = include_str!("../resources/i18n/en.json");
static AR_STRINGS: &str = include_str!("../resources/i18n/ar.json");
static DE_STRINGS: &str = include_str!("../resources/i18n/de.json");
static ES_STRINGS: &str = include_str!("../resources/i18n/es.json");
static FR_STRINGS: &str = include_str!("../resources/i18n/fr.json");
static HI_STRINGS: &str = include_str!("../resources/i18n/hi.json");
static ID_STRINGS: &str = include_str!("../resources/i18n/id.json");
static JA_STRINGS: &str = include_str!("../resources/i18n/ja.json");
static KO_STRINGS: &str = include_str!("../resources/i18n/ko.json");
static PT_BR_STRINGS: &str = include_str!("../resources/i18n/pt-BR.json");
static RU_STRINGS: &str = include_str!("../resources/i18n/ru.json");
static TR_STRINGS: &str = include_str!("../resources/i18n/tr.json");
static VI_STRINGS: &str = include_str!("../resources/i18n/vi.json");
static ZH_HANS_STRINGS: &str = include_str!("../resources/i18n/zh-Hans.json");
static ZH_HANT_STRINGS: &str = include_str!("../resources/i18n/zh-Hant.json");

static EN: OnceLock<Value> = OnceLock::new();
static AR: OnceLock<Value> = OnceLock::new();
static DE: OnceLock<Value> = OnceLock::new();
static ES: OnceLock<Value> = OnceLock::new();
static FR: OnceLock<Value> = OnceLock::new();
static HI: OnceLock<Value> = OnceLock::new();
static ID: OnceLock<Value> = OnceLock::new();
static JA: OnceLock<Value> = OnceLock::new();
static KO: OnceLock<Value> = OnceLock::new();
static PT_BR: OnceLock<Value> = OnceLock::new();
static RU: OnceLock<Value> = OnceLock::new();
static TR: OnceLock<Value> = OnceLock::new();
static VI: OnceLock<Value> = OnceLock::new();
static ZH_HANS: OnceLock<Value> = OnceLock::new();
static ZH_HANT: OnceLock<Value> = OnceLock::new();
static SYSTEM_LOCALE: OnceLock<&'static str> = OnceLock::new();

#[derive(serde::Serialize)]
pub struct LocaleOption {
    pub code: &'static str,
    pub name: &'static str,
    pub dir: &'static str,
}

pub const AVAILABLE_LOCALES: &[LocaleOption] = &[
    LocaleOption {
        code: "en",
        name: "English",
        dir: "ltr",
    },
    LocaleOption {
        code: "ar",
        name: "العربية",
        dir: "rtl",
    },
    LocaleOption {
        code: "de",
        name: "Deutsch",
        dir: "ltr",
    },
    LocaleOption {
        code: "es",
        name: "Español",
        dir: "ltr",
    },
    LocaleOption {
        code: "fr",
        name: "Français",
        dir: "ltr",
    },
    LocaleOption {
        code: "hi",
        name: "हिन्दी",
        dir: "ltr",
    },
    LocaleOption {
        code: "id",
        name: "Bahasa Indonesia",
        dir: "ltr",
    },
    LocaleOption {
        code: "ja",
        name: "日本語",
        dir: "ltr",
    },
    LocaleOption {
        code: "ko",
        name: "한국어",
        dir: "ltr",
    },
    LocaleOption {
        code: "pt-BR",
        name: "Português (Brasil)",
        dir: "ltr",
    },
    LocaleOption {
        code: "ru",
        name: "Русский",
        dir: "ltr",
    },
    LocaleOption {
        code: "tr",
        name: "Türkçe",
        dir: "ltr",
    },
    LocaleOption {
        code: "vi",
        name: "Tiếng Việt",
        dir: "ltr",
    },
    LocaleOption {
        code: "zh-Hans",
        name: "简体中文",
        dir: "ltr",
    },
    LocaleOption {
        code: "zh-Hant",
        name: "繁體中文",
        dir: "ltr",
    },
];

pub fn locale() -> &'static str {
    *SYSTEM_LOCALE.get_or_init(|| {
        std::env::var("LC_ALL")
            .ok()
            .or_else(|| std::env::var("LC_MESSAGES").ok())
            .or_else(|| std::env::var("LANG").ok())
            .and_then(|locale| choose_locale(&locale))
            .unwrap_or("en")
    })
}

pub fn strings() -> &'static Value {
    strings_static(locale())
}

pub fn strings_for(requested_locale: Option<&str>) -> (&'static str, Value) {
    let locale = requested_locale
        .and_then(choose_locale)
        .unwrap_or_else(locale);
    let mut strings = strings_static("en").clone();
    if locale != "en" {
        merge_json(&mut strings, strings_static(locale));
    }
    (locale, strings)
}

pub fn direction(locale: &str) -> &'static str {
    match locale {
        "ar" => "rtl",
        _ => "ltr",
    }
}

pub fn text(key: &str) -> String {
    lookup(strings(), key)
        .and_then(Value::as_str)
        .map(std::borrow::ToOwned::to_owned)
        .unwrap_or_else(|| key.to_owned())
}

fn lookup<'a>(root: &'a Value, dotted_key: &str) -> Option<&'a Value> {
    let mut node = root;
    for segment in dotted_key.split('.') {
        node = node.get(segment)?;
    }
    Some(node)
}

fn strings_static(locale: &str) -> &'static Value {
    match locale {
        "ar" => AR.get_or_init(|| parse(AR_STRINGS)),
        "de" => DE.get_or_init(|| parse(DE_STRINGS)),
        "es" => ES.get_or_init(|| parse(ES_STRINGS)),
        "fr" => FR.get_or_init(|| parse(FR_STRINGS)),
        "hi" => HI.get_or_init(|| parse(HI_STRINGS)),
        "id" => ID.get_or_init(|| parse(ID_STRINGS)),
        "ja" => JA.get_or_init(|| parse(JA_STRINGS)),
        "ko" => KO.get_or_init(|| parse(KO_STRINGS)),
        "pt-BR" => PT_BR.get_or_init(|| parse(PT_BR_STRINGS)),
        "ru" => RU.get_or_init(|| parse(RU_STRINGS)),
        "tr" => TR.get_or_init(|| parse(TR_STRINGS)),
        "vi" => VI.get_or_init(|| parse(VI_STRINGS)),
        "zh-Hans" => ZH_HANS.get_or_init(|| parse(ZH_HANS_STRINGS)),
        "zh-Hant" => ZH_HANT.get_or_init(|| parse(ZH_HANT_STRINGS)),
        _ => EN.get_or_init(|| parse(EN_STRINGS)),
    }
}

fn parse(input: &str) -> Value {
    serde_json::from_str(input).unwrap_or(Value::Null)
}

fn choose_locale(input: &str) -> Option<&'static str> {
    input.split(',').find_map(|candidate| {
        let candidate = candidate
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
            .replace('_', "-");
        match candidate.as_str() {
            "pt-br" => return Some("pt-BR"),
            "zh-cn" | "zh-sg" | "zh-my" | "zh-hans" => return Some("zh-Hans"),
            "zh-tw" | "zh-hk" | "zh-mo" | "zh-hant" => return Some("zh-Hant"),
            _ => {}
        }
        let language = candidate.split('-').next().unwrap_or("");
        match language {
            "ar" => Some("ar"),
            "en" => Some("en"),
            "de" => Some("de"),
            "es" => Some("es"),
            "fr" => Some("fr"),
            "hi" => Some("hi"),
            "id" => Some("id"),
            "ja" => Some("ja"),
            "ko" => Some("ko"),
            "pt" => Some("pt-BR"),
            "ru" => Some("ru"),
            "tr" => Some("tr"),
            "vi" => Some("vi"),
            "zh" => Some("zh-Hans"),
            _ => None,
        }
    })
}

fn merge_json(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, overlay_value) in overlay {
                match base.get_mut(key) {
                    Some(base_value) => merge_json(base_value, overlay_value),
                    None => {
                        base.insert(key.clone(), overlay_value.clone());
                    }
                }
            }
        }
        (base, overlay) => *base = overlay.clone(),
    }
}
