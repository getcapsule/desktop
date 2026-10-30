#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim().trim_start_matches('#');
        if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self {
            r: u8::from_str_radix(&s[0..2], 16).ok()?,
            g: u8::from_str_radix(&s[2..4], 16).ok()?,
            b: u8::from_str_radix(&s[4..6], 16).ok()?,
        })
    }

    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    pub fn mix(self, other: Rgb, t: f64) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let lerp = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as u8;
        Rgb { r: lerp(self.r, other.r), g: lerp(self.g, other.g), b: lerp(self.b, other.b) }
    }
}

fn channel(v: u8) -> f64 {
    let c = f64::from(v) / 255.0;
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn luminance(c: Rgb) -> f64 {
    0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
}

pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

use serde::Deserialize;

const MUTED_START: f64 = 0.55;
const RULE_T: f64 = 0.10;
const PLAYING_MIX: f64 = 0.12;
const PILL_MIX: f64 = 0.09;
pub const AA: f64 = 4.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Polarity {
    Light,
    Dark,
}

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("could not parse theme file: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("missing required field: {0}")]
    Missing(&'static str),
    #[error("field {0} is not a hex colour: {1}")]
    BadColour(&'static str, String),
}

#[derive(Debug, Deserialize)]
struct Wire {
    name: Option<String>,
    ground: Option<String>,
    panel: Option<String>,
    ink: Option<String>,
    accent: Option<String>,
    rule: Option<String>,
    muted: Option<String>,
    ok: Option<String>,
    warn: Option<String>,
    crit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub polarity: Polarity,
    pub ground: Rgb,
    pub panel: Rgb,
    pub rule: Rgb,
    pub ink: Rgb,
    pub muted: Rgb,
    pub muted2: Rgb,
    pub accent: Rgb,
    pub ok: Rgb,
    pub warn: Rgb,
    pub crit: Rgb,
}

fn required(field: &'static str, v: Option<String>) -> Result<Rgb, ThemeError> {
    let raw = v.ok_or(ThemeError::Missing(field))?;
    let parsed = Rgb::parse(&raw);
    parsed.ok_or(ThemeError::BadColour(field, raw))
}

fn optional(field: &'static str, v: Option<String>) -> Result<Option<Rgb>, ThemeError> {
    match v {
        None => Ok(None),
        Some(raw) => {
            let parsed = Rgb::parse(&raw);
            parsed.map(Some).ok_or(ThemeError::BadColour(field, raw))
        }
    }
}

fn derive_muted(ground: Rgb, panel: Rgb, ink: Rgb) -> Rgb {
    let mut t = MUTED_START;
    loop {
        let candidate = ground.mix(ink, t);
        let worst = contrast(candidate, ground).min(contrast(candidate, panel));
        if worst >= AA || t >= 1.0 {
            return candidate;
        }
        t += 0.01;
    }
}

fn derive_muted2(muted: Rgb, ink: Rgb, ground: Rgb, panel: Rgb, accent: Rgb) -> Rgb {
    let playing = ground.mix(accent, PLAYING_MIX);
    let pill = panel.mix(ink, PILL_MIX);
    let mut t = 0.0;
    loop {
        let candidate = muted.mix(ink, t);
        let worst = contrast(candidate, playing).min(contrast(candidate, pill));
        if worst >= AA || t >= 1.0 {
            return candidate;
        }
        t += 0.01;
    }
}

impl Theme {
    pub fn is_dark(&self) -> bool {
        self.polarity == Polarity::Dark
    }

    pub fn from_toml(id: &str, raw: &str) -> Result<Theme, ThemeError> {
        let w: Wire = toml::from_str(raw)?;

        let ground = required("ground", w.ground)?;
        let panel = required("panel", w.panel)?;
        let ink = required("ink", w.ink)?;
        let accent = required("accent", w.accent)?;

        let polarity = if luminance(ground) > 0.18 { Polarity::Light } else { Polarity::Dark };

        let rule = optional("rule", w.rule)?.unwrap_or_else(|| panel.mix(ink, RULE_T));
        let muted = optional("muted", w.muted)?.unwrap_or_else(|| derive_muted(ground, panel, ink));
        let muted2 = derive_muted2(muted, ink, ground, panel, accent);

        let (d_ok, d_warn, d_crit) = match polarity {
            Polarity::Dark => ("#62a688", "#c09a5f", "#c66d60"),
            Polarity::Light => ("#20613f", "#6f5115", "#993326"),
        };
        let fallback = |s: &str| Rgb::parse(s).expect("built-in default is valid");

        Ok(Theme {
            id: id.to_string(),
            name: w.name.unwrap_or_else(|| id.to_string()),
            polarity,
            ground,
            panel,
            rule,
            ink,
            muted,
            muted2,
            accent,
            ok: optional("ok", w.ok)?.unwrap_or_else(|| fallback(d_ok)),
            warn: optional("warn", w.warn)?.unwrap_or_else(|| fallback(d_warn)),
            crit: optional("crit", w.crit)?.unwrap_or_else(|| fallback(d_crit)),
        })
    }
}

pub const DEFAULT_THEME_ID: &str = "capsule";

const EMBEDDED: &[(&str, &str)] = &[
    ("capsule", include_str!("../themes/capsule.toml")),
    ("ember", include_str!("../themes/ember.toml")),
    ("paper", include_str!("../themes/paper.toml")),
    ("slate", include_str!("../themes/slate.toml")),
];

pub fn presets() -> Vec<Theme> {
    EMBEDDED
        .iter()
        .map(|(id, raw)| Theme::from_toml(id, raw).expect("shipped preset must parse"))
        .collect()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Failure {
    pub fg: String,
    pub bg: String,
    pub ratio: f64,
}

impl Theme {
    fn pairs(&self) -> Vec<(&'static str, Rgb, &'static str, Rgb)> {
        let base: [(&'static str, Rgb); 2] = [("ground", self.ground), ("panel", self.panel)];
        let raised: [(&'static str, Rgb); 4] = [
            ("playing row", self.ground.mix(self.accent, PLAYING_MIX)),
            ("playing row hover", self.ground.mix(self.accent, 0.16)),
            ("active pill", self.panel.mix(self.ink, PILL_MIX)),
            ("selected card", self.panel.mix(self.accent, 0.08)),
        ];

        let on_base: [(&'static str, Rgb); 6] = [
            ("ink", self.ink),
            ("muted", self.muted),
            ("accent", self.accent),
            ("ok", self.ok),
            ("warn", self.warn),
            ("crit", self.crit),
        ];
        let on_raised: [(&'static str, Rgb); 2] = [("ink", self.ink), ("muted2", self.muted2)];

        let mut out = Vec::new();
        for (bg_name, bg) in base {
            for (fg_name, fg) in on_base {
                out.push((fg_name, fg, bg_name, bg));
            }
        }
        for (bg_name, bg) in raised {
            for (fg_name, fg) in on_raised {
                out.push((fg_name, fg, bg_name, bg));
            }
        }
        out
    }

    pub fn failures(&self) -> Vec<Failure> {
        self.pairs()
            .into_iter()
            .filter_map(|(fg_name, fg, bg_name, bg)| {
                let ratio = contrast(fg, bg);
                (ratio < AA).then(|| Failure {
                    fg: fg_name.to_string(),
                    bg: bg_name.to_string(),
                    ratio: (ratio * 100.0).round() / 100.0,
                })
            })
            .collect()
    }

    pub fn css(&self) -> String {
        let tint = match self.polarity {
            Polarity::Dark => "255,255,255",
            Polarity::Light => "0,0,0",
        };
        format!(
            "--color-ground:{};--color-panel:{};--color-rule:{};--color-ink:{};\
             --color-muted:{};--color-muted2:{};--color-accent:{};--color-ok:{};\
             --color-warn:{};--color-crit:{};--edge-tint:{};--theme-color-scheme:{}",
            self.ground.hex(),
            self.panel.hex(),
            self.rule.hex(),
            self.ink.hex(),
            self.muted.hex(),
            self.muted2.hex(),
            self.accent.hex(),
            self.ok.hex(),
            self.warn.hex(),
            self.crit.hex(),
            tint,
            if self.is_dark() { "dark" } else { "light" }
        )
    }
}

use std::path::{Path, PathBuf};

const README: &str = r##"# Themes

Drop a `.toml` file in this folder and it appears in Settings under Appearance.
The filename is the theme's id, so `ember.toml` becomes `ember`.

Four colours are required:

```toml
name   = "Ember"
ground = "#14100d"
panel  = "#1f1a15"
ink    = "#d8cfc6"
accent = "#c9a184"
```

`rule`, `muted`, `ok`, `warn` and `crit` are optional and derived when absent.

Light or dark is worked out from `ground`, so a light background gives you a
light theme with no extra setting.

A file named after a built-in theme replaces it, which is a convenient way to
re-tune one you almost like.

capsule measures every colour pair it actually renders against WCAG AA (4.5:1)
and lists anything that falls short under the theme in Settings. A theme that
falls short still loads.
"##;

pub fn themes_dir(app_data: &Path) -> PathBuf {
    app_data.join("themes")
}

pub fn ensure_dir(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!(error = %e, path = %dir.display(), "could not create themes dir");
        return;
    }
    let readme = dir.join("README.md");
    if !readme.exists() {
        if let Err(e) = std::fs::write(&readme, README) {
            tracing::warn!(error = %e, "could not write themes README");
        }
    }
}

pub fn load_all(app_data: Option<&Path>) -> Vec<Theme> {
    let mut all = presets();

    let Some(app_data) = app_data else { return all };
    let dir = themes_dir(app_data);
    ensure_dir(&dir);

    let Ok(entries) = std::fs::read_dir(&dir) else { return all };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Ok(raw) = std::fs::read_to_string(&path) else { continue };

        match Theme::from_toml(id, &raw) {
            Ok(theme) => match all.iter().position(|t| t.id == theme.id) {
                Some(i) => {
                    tracing::info!(id, "user theme shadows a built-in");
                    all[i] = theme;
                }
                None => all.push(theme),
            },
            Err(e) => tracing::warn!(error = %e, path = %path.display(), "skipping theme"),
        }
    }

    all.sort_by(|a, b| a.id.cmp(&b.id));
    all
}

pub fn resolve(all: &[Theme], id: &str) -> Theme {
    if let Some(t) = all.iter().find(|t| t.id == id) {
        return t.clone();
    }
    tracing::warn!(id, "unknown theme; falling back to the default");
    all.iter()
        .find(|t| t.id == DEFAULT_THEME_ID)
        .cloned()
        .unwrap_or_else(|| presets().remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_controls_follow_theme_polarity_including_custom_themes() {
        for (ground, ink, expected) in [
            ("#f4f2ee", "#2a2724", "--theme-color-scheme:light"),
            ("#0d1014", "#ccd3db", "--theme-color-scheme:dark"),
        ] {
            let raw = format!(
                "name = \"Custom\"\nground = \"{ground}\"\npanel = \"{ground}\"\nink = \"{ink}\"\naccent = \"#9c8fc0\""
            );
            let theme = Theme::from_toml("custom", &raw).unwrap();
            assert!(theme.css().split(';').any(|rule| rule == expected));
        }
    }

    fn rgb(s: &str) -> Rgb {
        Rgb::parse(s).expect("valid hex")
    }

    #[test]
    fn parses_and_formats_hex() {
        assert_eq!(rgb("#0d1014"), Rgb { r: 13, g: 16, b: 20 });
        assert_eq!(rgb("0d1014"), Rgb { r: 13, g: 16, b: 20 });
        assert_eq!(rgb("#0D1014").hex(), "#0d1014");
        assert!(Rgb::parse("nope").is_none());
        assert!(Rgb::parse("#12345").is_none());
    }

    #[test]
    fn mixes_toward_the_second_colour() {
        let ground = rgb("#0d1014");
        let ink = rgb("#ccd3db");
        assert_eq!(ground.mix(ink, 0.0), ground);
        assert_eq!(ground.mix(ink, 1.0), ink);
        assert_eq!(ground.mix(ink, 0.18), rgb("#2f3338"));
    }

    #[test]
    fn reproduces_the_recorded_ink_ratio() {
        let r = contrast(rgb("#ccd3db"), rgb("#0d1014"));
        assert!((r - 12.6).abs() < 0.1, "ink on ground was {r}, comment records 12.6");
    }

    #[test]
    fn reproduces_the_recorded_muted2_ratios() {
        let ground = rgb("#0d1014");
        let panel = rgb("#151a1f");
        let ink = rgb("#ccd3db");
        let accent = rgb("#9c8fc0");
        let muted2 = rgb("#87929d");

        let playing = ground.mix(accent, 0.12);
        let pill = panel.mix(ink, 0.09);

        let on_playing = contrast(muted2, playing);
        let on_pill = contrast(muted2, pill);
        let on_ground = contrast(muted2, ground);

        assert!((on_playing - 5.17).abs() < 0.1, "playing row was {on_playing}");
        assert!((on_pill - 4.52).abs() < 0.1, "pill was {on_pill}");
        assert!((on_ground - 6.02).abs() < 0.1, "ground was {on_ground}");
    }

    const CAPSULE_MINIMAL: &str = r##"
name   = "capsule"
ground = "#0d1014"
panel  = "#151a1f"
ink    = "#ccd3db"
accent = "#9c8fc0"
"##;

    const CAPSULE_FULL: &str = r##"
name   = "capsule"
ground = "#0d1014"
panel  = "#151a1f"
rule   = "#242c35"
ink    = "#ccd3db"
muted  = "#78848f"
accent = "#9c8fc0"
ok     = "#62a688"
warn   = "#c09a5f"
crit   = "#c66d60"
"##;

    #[test]
    fn derived_optionals_are_readable_even_though_they_do_not_match_capsule() {
        let t = Theme::from_toml("capsule", CAPSULE_MINIMAL).expect("parses");
        assert!(contrast(t.muted, t.ground) >= AA, "derived muted on ground");
        assert!(contrast(t.muted, t.panel) >= AA, "derived muted on panel");
        assert!(contrast(t.rule, t.panel) < contrast(t.ink, t.panel), "rule is a quiet border");
    }

    #[test]
    fn muted2_is_derived_from_the_stated_muted_not_read_from_the_file() {
        let raw = format!("{CAPSULE_FULL}\nmuted2 = \"#ff0000\"\n");
        let t = Theme::from_toml("capsule", &raw).expect("parses");
        assert_eq!(t.muted2.hex(), "#87929d", "muted2 must be derived, not authorable");
    }

    #[test]
    fn stated_optionals_win_over_derivation() {
        let raw = format!("{CAPSULE_MINIMAL}\nrule = \"#010203\"\n");
        let t = Theme::from_toml("capsule", &raw).expect("parses");
        assert_eq!(t.rule.hex(), "#010203");
    }

    #[test]
    fn polarity_comes_from_ground_luminance() {
        let dark = Theme::from_toml("d", CAPSULE_MINIMAL).expect("parses");
        assert_eq!(dark.polarity, Polarity::Dark);
        assert!(dark.is_dark());

        let light = Theme::from_toml(
            "l",
            r##"
name   = "Paper"
ground = "#f4f2ee"
panel  = "#eae7e1"
ink    = "#2a2724"
accent = "#5a4b8a"
"##,
        )
        .expect("parses");
        assert_eq!(light.polarity, Polarity::Light);
        assert!(!light.is_dark());
    }

    #[test]
    fn muted2_derivation_clears_aa_on_both_raised_surfaces() {
        let t = Theme::from_toml("capsule", CAPSULE_FULL).expect("parses");
        let playing = t.ground.mix(t.accent, 0.12);
        let pill = t.panel.mix(t.ink, 0.09);
        assert!(contrast(t.muted2, playing) >= 4.5);
        assert!(contrast(t.muted2, pill) >= 4.5);
    }

    #[test]
    fn a_missing_required_field_names_the_field() {
        let raw = CAPSULE_MINIMAL.replace("ink    = \"#ccd3db\"\n", "");
        let err = Theme::from_toml("x", &raw).unwrap_err();
        assert!(err.to_string().contains("ink"), "got {err}");
    }

    #[test]
    fn a_bad_colour_names_the_field() {
        let raw = CAPSULE_MINIMAL.replace("#9c8fc0", "octarine");
        let err = Theme::from_toml("x", &raw).unwrap_err();
        assert!(err.to_string().contains("accent"), "got {err}");
    }

    #[test]
    fn malformed_toml_is_an_error_not_a_panic() {
        assert!(Theme::from_toml("x", "ground = [[[").is_err());
    }

    #[test]
    fn four_presets_ship_and_capsule_is_one_of_them() {
        let all = presets();
        assert_eq!(all.len(), 4);
        assert!(all.iter().any(|t| t.id == DEFAULT_THEME_ID));
    }

    #[test]
    fn capsule_reproduces_the_current_palette_exactly() {
        let t = presets().into_iter().find(|t| t.id == "capsule").expect("capsule");
        assert_eq!(t.ground.hex(), "#0d1014");
        assert_eq!(t.panel.hex(), "#151a1f");
        assert_eq!(t.rule.hex(), "#242c35");
        assert_eq!(t.ink.hex(), "#ccd3db");
        assert_eq!(t.muted.hex(), "#78848f");
        assert_eq!(t.muted2.hex(), "#87929d");
        assert_eq!(t.accent.hex(), "#9c8fc0");
        assert_eq!(t.ok.hex(), "#62a688");
        assert_eq!(t.warn.hex(), "#c09a5f");
        assert_eq!(t.crit.hex(), "#c66d60");
    }

    #[test]
    fn one_preset_is_light() {
        assert!(presets().iter().any(|t| t.polarity == Polarity::Light));
    }

    #[test]
    fn every_shipped_preset_clears_aa() {
        for t in presets() {
            let failures = t.failures();
            assert!(failures.is_empty(), "{} fails: {:?}", t.id, failures);
        }
    }

    #[test]
    fn css_carries_every_token_and_the_edge_tint() {
        let t = presets().into_iter().find(|t| t.id == "capsule").expect("capsule");
        let css = t.css();
        for token in [
            "--color-ground",
            "--color-panel",
            "--color-rule",
            "--color-ink",
            "--color-muted",
            "--color-muted2",
            "--color-accent",
            "--color-ok",
            "--color-warn",
            "--color-crit",
            "--edge-tint",
        ] {
            assert!(css.contains(token), "missing {token} in {css}");
        }
        assert!(css.contains("#0d1014"));
        assert!(!css.contains('{'), "css() emits declarations, not a rule block");
    }

    #[test]
    fn edge_tint_follows_polarity() {
        let dark = presets().into_iter().find(|t| t.id == "capsule").expect("capsule");
        assert!(dark.css().contains("--edge-tint:255,255,255"));

        let light = presets().into_iter().find(|t| t.id == "paper").expect("paper");
        assert!(light.css().contains("--edge-tint:0,0,0"));
    }

    #[test]
    fn a_failing_theme_reports_the_pair_and_ratio() {
        let raw = CAPSULE_FULL.replace("muted  = \"#78848f\"", "muted  = \"#1a1e22\"");
        let t = Theme::from_toml("bad", &raw).expect("parses");
        let f = t.failures();
        assert!(!f.is_empty(), "near-invisible muted must be reported");
        assert!(f.iter().all(|x| x.ratio < AA));
        assert!(f.iter().any(|x| x.fg == "muted"));
    }

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("capsule-theme-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_directory_is_created_with_a_readme() {
        let dir = tmpdir("create");
        let themes = themes_dir(&dir);
        assert!(!themes.exists());
        let all = load_all(Some(&dir));
        assert!(themes.is_dir(), "themes/ must be created");
        assert!(themes.join("README.md").is_file(), "README must be written");
        assert_eq!(all.len(), 4, "no user themes yet");
    }

    #[test]
    fn the_readme_is_not_loaded_as_a_theme() {
        let dir = tmpdir("readme");
        load_all(Some(&dir));
        let ids: Vec<String> = load_all(Some(&dir)).into_iter().map(|t| t.id).collect();
        assert!(!ids.iter().any(|i| i.contains("README")));
    }

    #[test]
    fn a_user_file_shadows_a_preset_of_the_same_id() {
        let dir = tmpdir("shadow");
        let themes = themes_dir(&dir);
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(
            themes.join("capsule.toml"),
            "name = \"Mine\"\nground = \"#000000\"\npanel = \"#111111\"\n\
             ink = \"#eeeeee\"\naccent = \"#8888ff\"\n",
        )
        .unwrap();

        let all = load_all(Some(&dir));
        assert_eq!(all.len(), 4, "shadowing must not add a fifth entry");
        let c = all.iter().find(|t| t.id == "capsule").expect("capsule");
        assert_eq!(c.name, "Mine");
        assert_eq!(c.ground.hex(), "#000000");
    }

    #[test]
    fn a_malformed_user_file_is_skipped_not_fatal() {
        let dir = tmpdir("malformed");
        let themes = themes_dir(&dir);
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(themes.join("broken.toml"), "ground = [[[").unwrap();
        std::fs::write(
            themes.join("good.toml"),
            "name = \"Good\"\nground = \"#0d1014\"\npanel = \"#151a1f\"\n\
             ink = \"#ccd3db\"\naccent = \"#9c8fc0\"\n",
        )
        .unwrap();

        let all = load_all(Some(&dir));
        assert!(all.iter().any(|t| t.id == "good"));
        assert!(!all.iter().any(|t| t.id == "broken"));
    }

    #[test]
    fn no_data_directory_still_yields_the_presets() {
        assert_eq!(load_all(None).len(), 4);
    }

    #[test]
    fn an_unknown_id_resolves_to_the_default() {
        let all = load_all(None);
        assert_eq!(resolve(&all, "nope").id, DEFAULT_THEME_ID);
        assert_eq!(resolve(&all, "paper").id, "paper");
    }

    #[test]
    fn the_name_falls_back_to_the_id() {
        let raw = CAPSULE_MINIMAL.replace("name   = \"capsule\"\n", "");
        let t = Theme::from_toml("ember", &raw).expect("parses");
        assert_eq!(t.name, "ember");
    }
}
