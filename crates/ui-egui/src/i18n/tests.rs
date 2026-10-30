//! Interface language tests (#8).

use super::*;

fn lang(code: &str) -> Lang {
    Lang::from_code(code).unwrap_or_else(|| panic!("{code} registered"))
}

/// Skips a test that needs the embedded catalogs when none were embedded (`CRAFT_I18N_DIR`
/// unset): the interface is then English only.
macro_rules! needs_catalogs {
    () => {
        if !catalogs_embedded() {
            return;
        }
    };
}

/// The translations in `code`'s embedded WordCraft catalog, plural forms apart: the fonts must
/// cover WordCraft's own wording. (The shared catalog holds every app's; the CJK checks read it too.)
fn translations(code: &str) -> Vec<String> {
    let code = lang(code).code();
    CRAFT_I18N
        .iter()
        .filter(|(c, ..)| *c == code)
        .flat_map(|(_, _, app)| app.lines())
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split('\t').nth(2))
        .flat_map(|t| t.split('|'))
        .map(String::from)
        .collect()
}

#[test]
fn system_locales_map_to_languages() {
    needs_catalogs!();
    let cases = [
        ("zh_CN.UTF-8", Some(lang("zh-hans"))),
        ("zh-CN", Some(lang("zh-hans"))),
        ("zh", Some(lang("zh-hans"))),
        ("zh-SG", Some(lang("zh-hans"))),
        ("zh-Hans-HK", Some(lang("zh-hans"))),
        // Traditional Chinese has no offered catalog, and a script is never crossed: English.
        ("zh_TW.UTF-8", None),
        ("zh-HK", None),
        ("zh-Hant", None),
        ("ja-JP", Some(lang("ja"))),
        ("ja_JP.eucJP@euro", Some(lang("ja"))),
        ("ko-KR", Some(lang("ko"))),
        ("ko_KR.UTF-8", Some(lang("ko"))),
        ("en-GB", Some(Lang::EN)),
        ("C", Some(Lang::EN)),
        ("POSIX", Some(Lang::EN)),
        ("fr-FR", Some(lang("fr"))),
        (UNSUPPORTED, None),
        ("", None),
        ("_", None),
        // Catalogs craft-i18n has but WordCraft does not offer stay in English.
        ("sr-Latn-RS", None),
        ("uk-UA", None),
        ("pl_PL.UTF-8", None),
        ("tr-TR", None),
        ("de", Some(lang("de"))),
        ("de-DE", Some(lang("de"))),
        ("de_AT.UTF-8", Some(lang("de"))),
        ("de-CH", Some(lang("de"))),
        ("DE_de.UTF-8@euro", Some(lang("de"))),
    ];
    for (tag, expected) in cases {
        assert_eq!(lang_from_tag(tag), expected, "locale {tag}");
    }
}

/// A language tag no catalog will ever have: `qaa`–`qtz` are reserved for local use (ISO 639-2),
/// so these tests keep working as real languages land.
const UNSUPPORTED: &str = "qaa-QM";

#[test]
fn the_first_supported_preferred_language_wins() {
    needs_catalogs!();
    // Windows and macOS list several preferred languages in order.
    assert_eq!(first_supported([UNSUPPORTED, "ko-KR", "en-US"]), Some(lang("ko")));
    assert_eq!(first_supported(["zh-Hant-TW", "ja-JP"]), Some(lang("ja")), "a language not offered is skipped");
    assert_eq!(first_supported(["en-US", "ja-JP"]), Some(Lang::EN));
    assert_eq!(first_supported([UNSUPPORTED, "qtz"]), None);
    assert_eq!(first_supported(["fr-FR", "de-DE"]), Some(lang("fr")));
    assert_eq!(first_supported([UNSUPPORTED, "de-DE"]), Some(lang("de")));
    assert_eq!(first_supported(["nl-NL", "it-IT"]), Some(lang("it")), "Dutch isn't offered, Italian is");
    assert_eq!(first_supported([]), None);
}

#[test]
fn settings_resolve_with_fallback() {
    needs_catalogs!();
    assert_eq!(Lang::from_pref("ja"), lang("ja"));
    assert_eq!(Lang::from_pref("ZH-HANS"), lang("zh-hans"));
    assert_eq!(Lang::from_pref("en"), Lang::EN);
    // `auto` and unknown codes follow the system (English under test).
    assert_eq!(Lang::from_pref(AUTO), Lang::EN);
    assert_eq!(Lang::from_pref("xx-unknown"), Lang::EN);
    assert_eq!(normalize_pref("Zh-Hans"), Some("zh-hans"));
    // A saved language that is no longer offered shows the system language, and isn't a setting.
    assert_eq!(Lang::from_pref("zh-hant"), Lang::EN);
    assert_eq!(normalize_pref("uk"), None);
    assert_eq!(normalize_pref("Auto"), Some(AUTO));
    assert_eq!(normalize_pref("zh"), None, "only exact codes are settings");
    assert_eq!(normalize_pref("xx"), None);
}

#[test]
fn lookups_fall_back_to_english() {
    let fx = fixture_lang("x-fixture", "\tFile\tFile-FX\n");
    assert_eq!(tr(fx, "File"), "File-FX");
    assert_eq!(tr(fx, "no such label"), "no such label");
    assert_eq!(tr(Lang::EN, "File"), "File");
    assert!(has(fx, "File") && !has(fx, "Save") && !has(Lang::EN, "File"));
}

#[test]
fn current_language_is_per_thread() {
    set_current(fixture_lang(
        "x-fixture",
        "\tInsert\tInsert-FX\n\tHome\tHome-FX\n\tFont\tFont-FX\n\tUndo\tUndo-FX\n\tUndo {action}\tUndo-FX: {action}\n\tBold\tBold-FX\n",
    ));
    assert_eq!(t("Insert"), "Insert-FX");
    assert_eq!(location("Home › Font"), "Home-FX › Font-FX");
    assert_eq!(prefixed("Undo", "Bold"), "Undo-FX: Bold-FX");
    assert_eq!(prefixed("Undo", ""), "Undo-FX");
    // An action without a translation keeps its English inside the translated template.
    assert_eq!(prefixed("Undo", "Typing"), "Undo-FX: Typing");
    std::thread::spawn(|| assert_eq!(current(), Lang::EN)).join().unwrap();
    set_current(Lang::EN);
    assert_eq!(t("Insert"), "Insert");
    assert_eq!(prefixed("Undo", "Typing"), "Undo Typing");
}

#[test]
fn placeholders_fill_without_reinterpreting_values() {
    let args = [("path", "a-{pages}.docx"), ("pages", "2")];
    assert_eq!(fmt("{pages}: {path}; {unknown}", &args), "2: a-{pages}.docx; {unknown}");
    assert_eq!(fmt("text {unfinished", &args), "text {unfinished");
    // A translation may put the placeholders in another order.
    let fx = fixture_lang("x-fixture", "\tPage {page} of {pages}\t{pages}: {page}\n");
    assert_eq!(fmt(tr(fx, "Page {page} of {pages}"), &[("page", "3"), ("pages", "9")]), "9: 3");
}

#[test]
fn the_language_setting_is_a_ui_command_and_persists() {
    needs_catalogs!();
    let mut app = crate::WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), Default::default());
    assert_eq!(app.ui.language, AUTO);
    let r = app.run("ui.language", serde_json::json!({"value": "ZH-HANS"})).unwrap();
    assert_eq!(app.ui.language, "zh-hans");
    assert_eq!(r["effective"], "zh-hans");
    assert!(r["available"].as_array().is_some_and(|a| a.len() == LANGUAGES.len()));
    assert!(app.run("ui.language", serde_json::json!({"value": "xx"})).is_err());
    assert_eq!(app.ui.language, "zh-hans", "an unknown code keeps the setting");
    let saved = serde_json::to_string(&app.ui).unwrap();
    let restored: crate::UiState = serde_json::from_str(&saved).unwrap();
    assert_eq!(restored.language, "zh-hans");
    // Settings saved before this option existed follow the system.
    let legacy: crate::UiState = serde_json::from_str(r#"{"tab":"Home"}"#).unwrap();
    assert_eq!(legacy.language, AUTO);
    app.run("ui.language", serde_json::json!({"value": "auto"})).unwrap();
    assert_eq!(app.ui.language, AUTO);
    // Reading without a value reports the current setting.
    assert_eq!(app.run("ui.language", serde_json::json!({})).unwrap()["language"], AUTO);
}

#[test]
fn brazilian_portuguese_locales_and_saved_preference_work_without_changing_the_document() {
    needs_catalogs!();
    let pt = lang("pt-br");
    for tag in ["pt-BR", "pt_BR.UTF-8", "PT-BR", "pt-BR-latn", "pt_BR.UTF-8@euro"] {
        assert_eq!(lang_from_tag(tag), Some(pt), "{tag}");
    }
    // European Portuguese and a bare `pt` have no catalog yet, so they follow the fallback
    // rather than the crate's `pt-br` match.
    for tag in ["pt-PT", "pt", "pt_PT.UTF-8", "pt-AO", "PT_pt"] {
        assert_eq!(lang_from_tag(tag), None, "{tag}");
    }
    assert_eq!(first_supported(["pt-PT", "fr-FR"]), Some(lang("fr")));
    assert_eq!(first_supported([UNSUPPORTED, "pt-BR", "en-US"]), Some(pt));
    let mut app = crate::WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), Default::default());
    let before = serde_json::to_value(&app.session.doc).unwrap();
    let result = app.run("ui.language", serde_json::json!({"value": "PT-BR"})).unwrap();
    assert_eq!(result["effective"], "pt-br");
    assert_eq!(app.ui.language, "pt-br");
    assert_eq!(serde_json::to_value(&app.session.doc).unwrap(), before);
    let saved = serde_json::to_string(&app.ui).unwrap();
    let restored: crate::UiState = serde_json::from_str(&saved).unwrap();
    assert_eq!(Lang::from_pref(&restored.language), pt);
    assert_eq!(pt.name(), "Português (Brasil)");
}

#[test]
fn bundled_interface_fonts_cover_brazilian_portuguese_without_system_fallbacks() {
    let db = wordcraft_fonts::FontDb::with_font_dirs(Vec::new());
    needs_catalogs!();
    let entries = translations("pt-br");
    let chars: std::collections::HashSet<char> = entries
        .iter()
        .flat_map(|e| e.chars())
        // The accented Latin letters Portuguese adds on top of ASCII; symbols such as ¶, ⌘ or 📂
        // come from the English sources and render as they already do.
        .filter(|c| ('\u{00C0}'..='\u{017F}').contains(c))
        .chain("ÀàÁáÂâÃãÇçÉéÊêÍíÓóÔôÕõÚúÜü".chars())
        .collect();
    for (family, style) in [("Inter", "Regular"), ("Inter", "Medium"), ("Inter", "SemiBold"), ("JetBrains Mono", "Regular")] {
        let face = db.face(family, style);
        assert_eq!(face.family, family, "must use the bundled interface face");
        for ch in &chars {
            assert_ne!(face.glyph_for(*ch), 0, "{family} {style} lacks {ch}");
        }
    }
}

#[test]
fn chinese_interface_fonts_load_and_cover_simplified_hanzi() {
    needs_catalogs!();
    // #241: without an embedded Chinese face the interface adds an installed CJK font; egui must
    // accept it (it panics on font data it can't parse) and draw simplified-only hanzi with it.
    let ctx = egui::Context::default();
    ctx.set_fonts(crate::theme::font_definitions(true, true));
    ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
    let covered = ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::proportional(14.0), "删除页选"));
    let (catalogs, _) = Lang::cjk_font_text(true);
    let embedded = !wordcraft_fonts::ui_needs_system_cjk(&catalogs, &wordcraft_fonts::ui_cjk_fonts(true));
    if embedded || wordcraft_fonts::system_cjk_ui_font(true, &['页']).is_some() {
        assert!(covered, "a Chinese face is available but the interface lacks simplified hanzi");
    }
}

#[test]
fn korean_interface_fonts_load_and_cover_hangul() {
    needs_catalogs!();
    let ctx = egui::Context::default();
    ctx.set_fonts(crate::theme::font_definitions_language(false, true, true));
    ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
    let covered = ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::proportional(14.0), "한글"));
    let (catalogs, names) = Lang::cjk_font_text_for(false, true);
    let mut texts = catalogs;
    texts.push(&names);
    let embedded = !wordcraft_fonts::ui_needs_system_cjk(&texts, &wordcraft_fonts::ui_cjk_fonts_for(false, true));
    if embedded || wordcraft_fonts::system_cjk_ui_font_for(false, true, &['한']).is_some() {
        assert!(covered, "a Korean face is available but the interface lacks Hangul glyphs");
    }
}

#[test]
fn cjk_catalogs_are_covered_by_the_embedded_interface_faces() {
    // #486: every CJK character of the Chinese, Japanese and Korean catalogs and language names
    // has a glyph in the interface font chain built from the embedded faces alone, in every
    // interface weight; `?` and boxes showed where the Japanese face was the only CJK one. Needs
    // a craft-fonts build (CRAFT_FONTS_DIR); without one there is nothing embedded to check, and
    // the interface reads an installed CJK font instead.
    if wordcraft_fonts::ui_cjk_fonts(false).is_empty() {
        eprintln!("skipped: built without craft-fonts (CRAFT_FONTS_DIR)");
        return;
    }
    needs_catalogs!();
    let names: String = Lang::all().map(Lang::name).collect();
    for code in ["zh-hans", "ja", "ko"] {
        let l = lang(code);
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::theme::font_definitions_language(l.prefers_hans(), l.prefers_korean(), false));
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        let entries = translations(code);
        let chars: std::collections::BTreeSet<char> =
            entries.iter().flat_map(|e| e.chars()).chain(names.chars()).filter(|c| wordcraft_fonts::is_cjk(*c)).collect();
        assert!(chars.len() > 300, "{code}: {} CJK characters", chars.len());
        for font in [crate::theme::regular(14.0), crate::theme::medium(14.0), crate::theme::semibold(14.0)] {
            let missing: String = ctx.fonts_mut(|f| chars.iter().filter(|c| !f.has_glyph(&font, **c)).collect());
            assert!(missing.is_empty(), "{code}: no glyph in {font:?} for {missing}");
        }
        // So the interface reads no installed CJK font for these languages.
        let mut texts: Vec<&str> = l.catalog_texts().collect();
        texts.push(&names);
        assert_eq!(
            wordcraft_fonts::ui_uncovered_cjk(&texts, &wordcraft_fonts::ui_cjk_fonts_for(l.prefers_hans(), l.prefers_korean())),
            Vec::<char>::new()
        );
    }
}

#[test]
fn only_cjk_languages_ask_for_an_installed_cjk_font() {
    needs_catalogs!();
    // #241 follow-up: an English interface must not read a large system CJK font on startup.
    let cjk: Vec<&str> = Lang::all().filter(|l| l.uses_cjk()).map(Lang::code).collect();
    assert_eq!(cjk, ["zh-hans", "ja", "ko"]);
}

#[test]
fn russian_locales_and_saved_preference_work_without_changing_the_document() {
    needs_catalogs!();
    let ru = lang("ru");
    for tag in ["ru", "ru-RU", "ru_RU.UTF-8", "RU-ru", "ru_UA.UTF-8", "ru-Cyrl-RU"] {
        assert_eq!(lang_from_tag(tag), Some(ru), "{tag}");
    }
    assert_eq!(first_supported([UNSUPPORTED, "ru-RU", "en-US"]), Some(ru));
    let mut app = crate::WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), Default::default());
    let before = serde_json::to_value(&app.session.doc).unwrap();
    let result = app.run("ui.language", serde_json::json!({"value": "RU"})).unwrap();
    assert_eq!(result["effective"], "ru");
    assert_eq!(app.ui.language, "ru");
    assert_eq!(serde_json::to_value(&app.session.doc).unwrap(), before);
    let saved = serde_json::to_string(&app.ui).unwrap();
    let restored: crate::UiState = serde_json::from_str(&saved).unwrap();
    assert_eq!(Lang::from_pref(&restored.language), ru);
    assert_eq!(ru.name(), "Русский");
}

#[test]
fn bundled_interface_fonts_cover_russian_without_system_fallbacks() {
    let db = wordcraft_fonts::FontDb::with_font_dirs(Vec::new());
    needs_catalogs!();
    let entries = translations("ru");
    let chars: std::collections::HashSet<char> = entries
        .iter()
        .flat_map(|e| e.chars())
        // Cyrillic letters plus the Russian quotes and dash; symbols such as ¶, ⌘ or 📂 come from
        // the English sources and render as they already do.
        .filter(|c| ('\u{0400}'..='\u{04FF}').contains(c) || matches!(c, '«' | '»' | '—'))
        .chain("АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдеёжзийклмнопрстуфхцчшщъыьэюя".chars())
        .collect();
    for (family, style) in [("Inter", "Regular"), ("Inter", "Medium"), ("Inter", "SemiBold"), ("JetBrains Mono", "Regular")] {
        let face = db.face(family, style);
        assert_eq!(face.family, family, "must use the bundled interface face");
        for ch in &chars {
            assert_ne!(face.glyph_for(*ch), 0, "{family} {style} lacks {ch}");
        }
    }
}

#[test]
fn built_in_style_names_are_translated_for_display_and_user_styles_are_not() {
    let sheet = wordcraft_doc::StyleSheet::builtin();
    let name = |id: &str| style_name(sheet.get(id).unwrap());
    set_current(fixture_lang(
        "x-fixture",
        "\tNormal\tNormal-FX\n\tHeading 1\tHeading-one-FX\n\tHeading {n}\tHeading-FX {n}\n\tTOC {n}\tTOC-FX {n}\n\tIndex {n}\tIndex-FX {n}\n",
    ));
    assert_eq!(name("Normal"), "Normal-FX");
    // An explicit entry wins over its family's template.
    assert_eq!(name("Heading1"), "Heading-one-FX");
    // Numbered families beyond the catalog's explicit entries go through one template.
    assert_eq!(name("Heading7"), "Heading-FX 7");
    assert_eq!(name("TOC9"), "TOC-FX 9");
    assert_eq!(builtin_style_name("Index 2"), "Index-FX 2");
    // A style the author made keeps its name, even when it matches a built-in one.
    let mine = wordcraft_doc::Style { name: "Normal".into(), builtin: false, ..Default::default() };
    assert_eq!(style_name(&mine), "Normal");
    // A built-in name without a translation stays English.
    assert_eq!(builtin_style_name("Some Future Style"), "Some Future Style");
    assert_eq!(builtin_style_name("Heading x"), "Heading x");
    set_current(Lang::EN);
    assert_eq!(name("Heading7"), "Heading 7");
    // The document itself never changes.
    assert_eq!(sheet.get("Normal").unwrap().name, "Normal");
}

#[test]
fn context_entries_override_only_their_context() {
    let fx = fixture_lang("x-fixture", "\tTitle\tTitle-FX\npreview\tTitle\tSample-FX\n");
    assert_eq!(tr(fx, "Title"), "Title-FX");
    assert_eq!(trc(fx, "preview", "Title"), "Sample-FX");
    assert_eq!(trc(fx, "template", "Title"), "Title-FX");
    assert_eq!(trc(Lang::EN, "preview", "Title"), "Title");
    assert_eq!(trc(fx, "preview", "Unknown"), "Unknown");
}

#[test]
fn french_locales_and_saved_preference_work_without_changing_the_document() {
    needs_catalogs!();
    let fr = lang("fr");
    for tag in ["fr", "fr-FR", "FR_fr.UTF-8", "fr-CA", "fr_BE.UTF-8@euro", "fr-CH", "fr-Latn-FR"] {
        assert_eq!(lang_from_tag(tag), Some(fr), "{tag}");
    }
    assert_eq!(first_supported([UNSUPPORTED, "fr-CA", "en-US"]), Some(fr));
    assert_eq!(normalize_pref("FR"), Some("fr"));
    assert_eq!(fr.name(), "Français");
    let mut app = crate::WordApp::new(wordcraft_engine::Session::new(wordcraft_engine::sample::sample_document()), Default::default());
    let before = serde_json::to_value(&app.session.doc).unwrap();
    let result = app.run("ui.language", serde_json::json!({"value": "FR"})).unwrap();
    assert_eq!(result["effective"], "fr");
    assert_eq!(app.ui.language, "fr");
    assert!(result["available"].as_array().unwrap().iter().any(|l| l["code"] == "fr" && l["name"] == "Français"));
    assert_eq!(serde_json::to_value(&app.session.doc).unwrap(), before);
    let saved = serde_json::to_string(&app.ui).unwrap();
    let restored: crate::UiState = serde_json::from_str(&saved).unwrap();
    assert_eq!(Lang::from_pref(&restored.language), fr);
    assert!(app.run("ui.language", serde_json::json!({"value": "fr-unknown"})).is_err());
    assert_eq!(app.ui.language, "fr", "invalid preferences must keep the saved language");
    app.run("ui.language", serde_json::json!({"value": "auto"})).unwrap();
    assert_eq!(app.ui.language, AUTO);
}

#[test]
fn bundled_interface_fonts_cover_french_without_system_fallbacks() {
    let db = wordcraft_fonts::FontDb::with_font_dirs(Vec::new());
    needs_catalogs!();
    let entries = translations("fr");
    // French letters and punctuation: accents, ligatures, guillemets and the typographic apostrophe.
    let extra = "ÀàÂâÆæÇçÉéÈèÊêËëÎîÏïÔôŒœÙùÛûÜüŸÿ«»’";
    let chars: std::collections::HashSet<char> =
        entries.iter().flat_map(|e| e.chars()).filter(|c| ('\u{00C0}'..='\u{017F}').contains(c)).chain(extra.chars()).collect();
    for (family, style) in [("Inter", "Regular"), ("Inter", "Medium"), ("Inter", "SemiBold"), ("JetBrains Mono", "Regular")] {
        let face = db.face(family, style);
        assert_eq!(face.family, family, "must use the bundled interface face");
        for ch in &chars {
            assert_ne!(face.glyph_for(*ch), 0, "{family} {style} lacks {ch:?}");
        }
    }
    // The no-break spaces French puts before ; : ! ? and inside « » (the catalog uses plain
    // spaces today) are in the interface faces too.
    for style in ["Regular", "Medium", "SemiBold"] {
        let face = db.face("Inter", style);
        for ch in ['\u{00A0}', '\u{202F}'] {
            assert_ne!(face.glyph_for(ch), 0, "Inter {style} lacks {ch:?}");
        }
    }
    assert!(!lang("fr").uses_cjk());
}

#[test]
fn an_italian_interface_writes_new_documents_in_italian() {
    needs_catalogs!();
    let mut app = crate::WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), Default::default());
    let ctx = egui::Context::default();
    let frame = |app: &mut crate::WordApp| ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx())).drop_without_applying_deltas();
    frame(&mut app);
    app.run("ui.language", serde_json::json!({"value": "it"})).unwrap();
    frame(&mut app);
    assert_eq!(app.session.editing_lang.as_deref(), Some("it-IT"));
    assert_eq!(app.session.doc.styles.default_chr.lang.as_deref(), Some("it-IT"), "the untouched first document follows");
    // Typing Italian then shows no English misspellings; the status bar names the language, translated.
    app.session.run("text.insert", &serde_json::json!({"text": "Città vecchie e caffè"})).unwrap();
    let issues = app.session.run("review.issues", &serde_json::json!({})).unwrap();
    assert!(issues.as_array().unwrap().iter().all(|i| i["lang"] == "it"), "{issues}");
    // The app's frames set the interface language; name it in English first.
    set_current(Lang::EN);
    assert_eq!(crate::ribbon::caret_language(&app), "Italian");
    super::pseudo::set(true);
    assert_eq!(crate::ribbon::caret_language(&app), "⟦Italian⟧", "the language name goes through the catalog");
    super::pseudo::set(false);
}

#[test]
fn the_editing_languages_are_the_ones_wordcraft_can_proof() {
    let with: Vec<(&str, &str)> = LANGUAGES.iter().filter_map(|l| l.editing_lang.map(|e| (l.code, e))).collect();
    assert_eq!(with, [("pt-br", "pt-BR"), ("it", "it-IT")]);
}

#[test]
fn german_locales_and_saved_preference_keep_document_content() {
    needs_catalogs!();
    let de = lang("de");
    for tag in ["de", "de-DE", "de-AT", "de-CH", "DE_de.UTF-8", "de_DE.UTF-8@euro", "de-Latn-DE"] {
        assert_eq!(lang_from_tag(tag), Some(de), "{tag}");
    }
    assert_eq!(first_supported([UNSUPPORTED, "de-AT", "en-US"]), Some(de));
    assert_eq!(normalize_pref("DE"), Some("de"));
    assert_eq!(de.name(), "Deutsch");
    let mut app = crate::WordApp::new(wordcraft_engine::Session::new(wordcraft_engine::sample::sample_document()), Default::default());
    let before = serde_json::to_value(&app.session.doc).unwrap();
    let result = app.run("ui.language", serde_json::json!({"value": "DE"})).unwrap();
    assert_eq!(result["effective"], "de");
    assert_eq!(app.ui.language, "de");
    assert_eq!(serde_json::to_value(&app.session.doc).unwrap(), before);
    let saved = serde_json::to_string(&app.ui).unwrap();
    let restored: crate::UiState = serde_json::from_str(&saved).unwrap();
    assert_eq!(Lang::from_pref(&restored.language), de);
}

#[test]
fn bundled_interface_fonts_cover_german_letters() {
    let db = wordcraft_fonts::FontDb::with_font_dirs(Vec::new());
    for (family, style) in [("Inter", "Regular"), ("Inter", "Medium"), ("Inter", "SemiBold"), ("JetBrains Mono", "Regular")] {
        let face = db.face(family, style);
        assert_eq!(face.family, family, "must use the bundled interface face");
        for ch in "ÄäÖöÜüß".chars() {
            assert_ne!(face.glyph_for(ch), 0, "{family} {style} lacks {ch}");
        }
    }
}

#[test]
fn italian_locales_and_saved_preference_keep_document_content() {
    needs_catalogs!();
    let it = lang("it");
    for tag in ["it", "it-IT", "it_IT.UTF-8", "it-CH", "it_IT.UTF-8@euro"] {
        assert_eq!(lang_from_tag(tag), Some(it), "{tag}");
    }
    assert_eq!(first_supported([UNSUPPORTED, "it-IT", "en-US"]), Some(it));
    assert_eq!(normalize_pref("IT"), Some("it"));
    assert_eq!(it.name(), "Italiano");
    let mut app = crate::WordApp::new(wordcraft_engine::Session::new(wordcraft_engine::sample::sample_document()), Default::default());
    let before = serde_json::to_value(&app.session.doc).unwrap();
    let result = app.run("ui.language", serde_json::json!({"value": "IT"})).unwrap();
    assert_eq!(result["effective"], "it");
    assert_eq!(app.ui.language, "it");
    assert_eq!(serde_json::to_value(&app.session.doc).unwrap(), before);
    let saved = serde_json::to_string(&app.ui).unwrap();
    let restored: crate::UiState = serde_json::from_str(&saved).unwrap();
    assert_eq!(Lang::from_pref(&restored.language), it);
}

#[test]
fn bundled_interface_fonts_cover_italian_letters() {
    let db = wordcraft_fonts::FontDb::with_font_dirs(Vec::new());
    for (family, style) in [("Inter", "Regular"), ("Inter", "Medium"), ("Inter", "SemiBold"), ("JetBrains Mono", "Regular")] {
        let face = db.face(family, style);
        assert_eq!(face.family, family, "must use the bundled interface face");
        for ch in "ÀàÈèÉéÌìÒòÙù".chars() {
            assert_ne!(face.glyph_for(ch), 0, "{family} {style} lacks {ch}");
        }
    }
}

/// #3317: the spooler's English receipt is not spliced into a translated sentence.
#[test]
fn spooled_print_receipt_is_translated() {
    use crate::printing::{Feedback, feedback};
    let fx = fixture_lang(
        "x-fixture",
        "\tPrint job submitted: spooled {count} sheet(s)\tSpooled-FX: {count}\n\tPrint job submitted: {receipt}\tSubmitted-FX: {receipt}\n",
    );
    let receipt = || Feedback::Submitted("spooled 2 sheet(s)".into());
    assert_eq!(feedback(&receipt(), |s| tr(fx, s).to_owned()), "Spooled-FX: 2");
    assert_eq!(feedback(&receipt(), str::to_owned), "Print job submitted: spooled 2 sheet(s)");
}

/// The "Open" paragraph-spacing preset is roomy spacing, not the file verb: only a `spacing` row
/// translates it.
#[test]
fn a_context_lookup_never_borrows_the_plain_translation() {
    set_current(fixture_lang("x-fixture", "\tOpen\tOpen-file-FX\n"));
    assert_eq!(t("Open"), "Open-file-FX");
    assert_eq!(get("spacing", "Open"), None);
    set_current(fixture_lang("x-fixture", "\tOpen\tOpen-file-FX\nspacing\tOpen\tRoomy-FX\n"));
    assert_eq!(get("spacing", "Open"), Some("Roomy-FX"));
    set_current(Lang::EN);
    assert_eq!(get("spacing", "Open"), None);
}

/// The assistant's Generate/Discard/Original rows sit in the `assistant` context: they win there,
/// and the plain rows that other dialogs use keep their shared wording.
#[test]
fn assistant_context_rows_never_replace_the_shared_plain_rows() {
    let fx = fixture_lang(
        "x-fixture",
        "\tGenerate\tShared-Generate-FX\n\tDiscard\tShared-Discard-FX\n\tOriginal\tShared-Original-FX\n\
         assistant\tGenerate\tAssist-Generate-FX\nassistant\tDiscard\tAssist-Discard-FX\nassistant\tOriginal\tAssist-Original-FX\n",
    );
    assert_eq!(trc(fx, "assistant", "Generate"), "Assist-Generate-FX");
    assert_eq!(trc(fx, "assistant", "Discard"), "Assist-Discard-FX");
    assert_eq!(trc(fx, "assistant", "Original"), "Assist-Original-FX");
    assert_eq!(tr(fx, "Generate"), "Shared-Generate-FX");
    assert_eq!(tr(fx, "Discard"), "Shared-Discard-FX");
    assert_eq!(tr(fx, "Original"), "Shared-Original-FX");
}

/// Every embedded catalog has its own `assistant` rows for those three labels.
#[test]
fn every_catalog_has_assistant_rows_for_generate_discard_and_original() {
    needs_catalogs!();
    for l in Lang::all() {
        if l.code() == "en" {
            continue;
        }
        for s in ["Generate", "Discard", "Original"] {
            assert!(l.i18n().get(l.lang, "assistant", s).is_some(), "{} lacks assistant row for {s}", l.code());
        }
    }
}

/// WordCraft offers exactly the languages in `LANGUAGES` when craft-i18n is embedded, and English
/// alone when it isn't: a missing catalog never fails, it shows English.
#[test]
fn the_languages_offered_are_the_embedded_ones_of_the_list() {
    let all: Vec<&str> = Lang::all().map(Lang::code).collect();
    if CRAFT_I18N.is_empty() {
        assert_eq!(all, ["en"]);
        assert_eq!(Lang::from_code("fr"), None);
        assert_eq!(Lang::from_pref("fr"), Lang::EN, "a saved language without a catalog shows English");
        assert_eq!(normalize_pref("fr"), None);
        assert_eq!(tr(Lang::EN, "File"), "File");
        return;
    }
    let listed: Vec<&str> = LANGUAGES.iter().map(|l| l.code).collect();
    assert_eq!(all, listed, "every language WordCraft lists has its catalog in craft-i18n");
    assert!(Lang::from_code("pseudo").is_none(), "the pseudo locale is never a setting");
}

/// The flatpak manifest builds with the craft-i18n commit `craft-i18n.rev` pins (CI and releases
/// read the file; the manifest has to name it itself).
#[test]
fn the_flatpak_manifest_pins_the_craft_i18n_rev() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let rev = std::fs::read_to_string(root.join("craft-i18n.rev")).unwrap();
    let rev = rev.trim();
    assert!(rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()), "craft-i18n.rev holds one full SHA: {rev:?}");
    let manifest = std::fs::read_to_string(root.join("packaging/linux/flatpak/ai.storyteller.wordcraft.yml")).unwrap();
    let source = manifest.split("url: https://github.com/storytold/craft-i18n").nth(1).expect("a craft-i18n git source");
    let source = source.split("- type:").next().unwrap_or(source);
    let commit = source.lines().find_map(|l| l.trim().strip_prefix("commit:")).expect("the craft-i18n source pins a commit");
    assert_eq!(commit.trim().trim_matches(['\'', '"']), rev);
    assert!(manifest.contains("CRAFT_I18N_DIR: /run/build/wordcraft/craft-i18n"), "the build embeds the catalogs");
}
