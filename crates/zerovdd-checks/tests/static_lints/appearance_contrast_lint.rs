use std::collections::BTreeMap;
use std::fs;

use regex::Regex;
use zerovdd_checks::repo_root;

const MIN_WCAG_AA_CONTRAST: f64 = 4.5;

fn parse_hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let digits = hex.strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&digits[0..2], 16).ok()?;
    let g = u8::from_str_radix(&digits[2..4], 16).ok()?;
    let b = u8::from_str_radix(&digits[4..6], 16).ok()?;
    Some((r, g, b))
}

fn srgb_channel_luminance(channel: u8) -> f64 {
    let c = f64::from(channel) / 255.0;
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn relative_luminance((r, g, b): (u8, u8, u8)) -> f64 {
    0.2126 * srgb_channel_luminance(r)
        + 0.7152 * srgb_channel_luminance(g)
        + 0.0722 * srgb_channel_luminance(b)
}

fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn extract_block_vars(css: &str, selector: &str) -> BTreeMap<String, (u8, u8, u8)> {
    let mut vars = BTreeMap::new();
    let var_re = Regex::new(r"(--zv-[a-z0-9-]+)\s*:\s*(#[0-9A-Fa-f]{6})\s*;").expect("var regex");

    let mut search_from = 0usize;
    while let Some(pos) = css[search_from..].find(selector) {
        let abs = search_from + pos;
        let after = &css[abs + selector.len()..];
        let trimmed = after.trim_start();
        if let Some(body_start) = trimmed.strip_prefix('{') {
            if let Some(close_idx) = body_start.find('}') {
                let body = &body_start[..close_idx];
                for cap in var_re.captures_iter(body) {
                    if let Some(rgb) = parse_hex_rgb(&cap[2]) {
                        vars.insert(cap[1].to_string(), rgb);
                    }
                }
            }
        }
        search_from = abs + selector.len();
    }
    vars
}

fn check_theme_contrast(css: &str) -> Vec<String> {
    let mut violations = Vec::new();
    let pairs = [
        ("--zv-fg", "--zv-bg"),
        ("--zv-fg", "--zv-surface"),
        ("--zv-fg", "--zv-surface-2"),
        ("--zv-muted", "--zv-bg"),
        ("--zv-muted", "--zv-surface"),
        ("--zv-accent", "--zv-bg"),
        ("--zv-accent", "--zv-surface"),
        ("--zv-on-accent", "--zv-accent"),
    ];

    for (theme_name, selector) in [("dark", ":root"), ("light", ":root[data-theme=\"light\"]")] {
        let vars = extract_block_vars(css, selector);
        for (fg_key, bg_key) in pairs {
            let Some(&fg) = vars.get(fg_key) else {
                violations.push(format!("{theme_name} theme missing hex token `{fg_key}`"));
                continue;
            };
            let Some(&bg) = vars.get(bg_key) else {
                violations.push(format!("{theme_name} theme missing hex token `{bg_key}`"));
                continue;
            };
            let ratio = contrast_ratio(fg, bg);
            if ratio < MIN_WCAG_AA_CONTRAST {
                violations.push(format!(
                    "{theme_name} theme contrast failure: `{fg_key}` on `{bg_key}` is {ratio:.2}:1 (minimum {MIN_WCAG_AA_CONTRAST:.1}:1)"
                ));
            }
        }
    }

    violations
}

#[test]
fn starlight_theme_tokens_meet_wcag_aa_contrast() {
    let theme_path = repo_root().join("docs/src/styles/zerovdd-theme.css");
    let css = fs::read_to_string(&theme_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", theme_path.display()));
    let violations = check_theme_contrast(&css);
    assert!(
        violations.is_empty(),
        "appearance_contrast_lint violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rejects_low_contrast_theme_pairs_and_accepts_accessible_pairs() {
    let bad_css = concat!(
        ":root {\n",
        "  --zv-bg: #111111;\n",
        "  --zv-surface: #222222;\n",
        "  --zv-surface-2: #333333;\n",
        "  --zv-fg: #444444;\n",
        "  --zv-muted: #333333;\n",
        "  --zv-accent: #225577;\n",
        "  --zv-on-accent: #111111;\n",
        "}\n",
        ":root[data-theme=\"light\"] {\n",
        "  --zv-bg: #FFFFFF;\n",
        "  --zv-surface: #FFFFFF;\n",
        "  --zv-surface-2: #F0F0F0;\n",
        "  --zv-fg: #999999;\n",
        "  --zv-muted: #BBBBBB;\n",
        "  --zv-accent: #77CCEE;\n",
        "  --zv-on-accent: #FFFFFF;\n",
        "}\n",
    );
    assert!(
        !check_theme_contrast(bad_css).is_empty(),
        "expected low-contrast dark and light theme pairs to fail WCAG AA check"
    );

    let good_css = concat!(
        ":root {\n",
        "  --zv-bg: #0A0A0A;\n",
        "  --zv-surface: #16181D;\n",
        "  --zv-surface-2: #242730;\n",
        "  --zv-fg: #F4F6F8;\n",
        "  --zv-muted: #9CA3AF;\n",
        "  --zv-accent: #38BDF8;\n",
        "  --zv-on-accent: #0A0A0A;\n",
        "}\n",
        ":root[data-theme=\"light\"] {\n",
        "  --zv-bg: #F8FAFC;\n",
        "  --zv-surface: #FFFFFF;\n",
        "  --zv-surface-2: #EEF2F6;\n",
        "  --zv-fg: #111827;\n",
        "  --zv-muted: #4B5563;\n",
        "  --zv-accent: #0369A1;\n",
        "  --zv-on-accent: #FFFFFF;\n",
        "}\n",
    );
    let violations = check_theme_contrast(good_css);
    assert!(
        violations.is_empty(),
        "expected accessible theme pairs to pass, got: {violations:?}"
    );
}
