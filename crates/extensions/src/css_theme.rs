//! Best-effort conversion of a Discord client CSS theme (Vencord/BetterDiscord) into a native
//! theme draft. Only custom properties are read: selectors that restyle Discord's web DOM have
//! no native equivalent and are counted as ignored. Nothing is fetched; a remote background
//! image or font is reported to the caller, which decides what to do with it.
use crate::{
	API_VERSION, Background, BackgroundFit, BackgroundTarget, Error, ExtensionKind, Manifest,
	Package, SectionOpacity, Theme, ThemePalette, valid_https_url,
};
use std::collections::BTreeMap;

pub const MAX_CSS_BYTES: usize = 1024 * 1024;
const MAX_DECLARATIONS: usize = 8192;
const MAX_VALUE_BYTES: usize = 1024;
const MAX_VAR_DEPTH: usize = 8;
const MAX_FONT_FAMILIES: usize = 16;

/// The draft and what the import could not carry over.
#[derive(Clone, Debug)]
pub struct CssTheme {
	pub package: Package,
	/// Requested interface families in CSS order, without generic families.
	pub fonts: Vec<String>,
	/// HTTPS background named by the theme. It is reported, never downloaded.
	pub background_url: Option<String>,
	/// Native theme values set from the CSS.
	pub mapped: usize,
	/// Rules, declarations and imports that only target Discord's web interface.
	pub ignored: usize,
}

/// Native colors and the CSS variables that supply them, most specific first. The first set
/// covers ClearVision-style settings, the rest Discord's own design tokens that most client
/// themes override.
const COLORS: &[(&str, &[&str])] = &[
	(
		"accent",
		&["--main-color", "--brand-experiment", "--brand-500"],
	),
	(
		"link",
		&["--text-link", "--main-color", "--brand-experiment"],
	),
	(
		"selected",
		&["--channel-selected-bg", "--background-modifier-selected"],
	),
	(
		"positive",
		&["--success-color", "--status-positive", "--text-positive"],
	),
	(
		"warning",
		&["--idle-color", "--status-warning", "--text-warning"],
	),
	(
		"danger",
		&["--danger-color", "--status-danger", "--text-danger"],
	),
	(
		"text",
		&["--normal-text", "--text-normal", "--text-default"],
	),
	("text_strong", &["--header-primary", "--text-strong"]),
	("muted", &["--muted-text", "--text-muted"]),
	(
		"base",
		&["--background-tertiary", "--background-base-lowest"],
	),
	(
		"sidebar",
		&["--background-secondary", "--background-base-lower"],
	),
	("chat", &["--background-primary", "--background-base-low"]),
	(
		"raised",
		&["--background-floating", "--background-surface-high"],
	),
	("hover", &["--background-modifier-hover"]),
	(
		"border",
		&["--background-modifier-accent", "--border-subtle"],
	),
];
const FONT_VARS: &[&str] = &["--main-font", "--font-primary", "--font-display"];
const IMAGE_VARS: &[&str] = &["--background-image", "--app-background", "--bg-image"];
const GENERIC_FONTS: &[&str] = &[
	"serif",
	"sans-serif",
	"monospace",
	"cursive",
	"fantasy",
	"system-ui",
	"ui-sans-serif",
	"ui-serif",
	"ui-monospace",
	"ui-rounded",
	"emoji",
	"math",
	"inherit",
	"initial",
	"unset",
];

type Rgba = [u8; 4];

#[derive(Clone, Copy, PartialEq)]
enum Scope {
	Root,
	Dark,
	Light,
}

/// Convert `css` into a theme package with the caller's local identity.
pub fn import_css(css: &str, id: String) -> Result<CssTheme, Error> {
	if css.len() > MAX_CSS_BYTES {
		return Err(Error::Limit);
	}
	let mut manifest = Manifest {
		api_version: API_VERSION,
		id,
		name: "Imported theme".into(),
		version: "1.0.0".into(),
		author: "Unknown".into(),
		license: "NOASSERTION".into(),
		source: String::new(),
		kind: ExtensionKind::Theme,
		capabilities: Vec::new(),
		actions: Vec::new(),
	};
	metadata(css, &mut manifest);
	let css = strip_comments(css);
	let mut scopes: [BTreeMap<String, String>; 3] = Default::default();
	let mut ignored = css.matches("@import").count();
	let mut declarations = 0;
	for (selector, body) in leaf_blocks(&css) {
		let scope = scope(selector);
		for declaration in body.split(';') {
			let Some((name, value)) = declaration.split_once(':') else {
				continue;
			};
			let (name, value) = (name.trim(), value.trim());
			if name.is_empty() {
				continue;
			}
			declarations += 1;
			if declarations > MAX_DECLARATIONS {
				return Err(Error::Limit);
			}
			match scope {
				Some(scope) if name.starts_with("--") && value.len() <= MAX_VALUE_BYTES => {
					let value = value.trim_end_matches("!important").trim();
					scopes[scope as usize].insert(name.to_owned(), value.to_owned());
				}
				_ => ignored += 1,
			}
		}
	}
	let [root, dark, light] = scopes;
	let dark = merged(&root, &dark);
	let light = merged(&root, &light);
	let (dark_palette, dark_count) = palette(&dark, true);
	let (light_palette, light_count) = palette(&light, false);
	let fonts = font_families(&dark);
	let background_url = background_url(&dark);
	let mapped = dark_count.max(light_count)
		+ usize::from(!fonts.is_empty())
		+ usize::from(background_url.is_some());
	if dark_count.max(light_count) == 0 {
		return Err(Error::Invalid);
	}
	let package = Package {
		background_image: Vec::new(),
		cover_image: Vec::new(),
		font: Vec::new(),
		manifest,
		theme: Some(Theme {
			light: light_palette,
			dark: dark_palette,
			style: Default::default(),
		}),
		wasm: Vec::new(),
	};
	package.validate()?;
	Ok(CssTheme {
		package,
		fonts,
		background_url,
		mapped,
		ignored,
	})
}

fn merged(
	root: &BTreeMap<String, String>,
	theme: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
	let mut scope = root.clone();
	scope.extend(theme.iter().map(|(k, v)| (k.clone(), v.clone())));
	scope
}

fn strip_comments(css: &str) -> String {
	let mut out = String::with_capacity(css.len());
	let mut rest = css;
	while let Some(start) = rest.find("/*") {
		out.push_str(&rest[..start]);
		rest = rest[start + 2..]
			.find("*/")
			.map_or("", |end| &rest[start + 2 + end + 2..]);
	}
	out.push_str(rest);
	out
}

/// Innermost `selector { body }` pairs, including those nested in `@media`. Unbalanced input
/// yields only the complete blocks.
fn leaf_blocks(css: &str) -> Vec<(&str, &str)> {
	let mut blocks = Vec::new();
	// (selector start, body start, has nested block)
	let mut stack: Vec<(usize, usize, bool)> = Vec::new();
	let mut segment = 0;
	for (index, byte) in css.bytes().enumerate() {
		match byte {
			b'{' => {
				if let Some(parent) = stack.last_mut() {
					parent.2 = true;
				}
				stack.push((segment, index + 1, false));
				segment = index + 1;
			}
			b'}' => {
				if let Some((selector, body, nested)) = stack.pop()
					&& !nested
				{
					blocks.push((css[selector..body - 1].trim(), &css[body..index]));
				}
				segment = index + 1;
			}
			b';' if stack.last().is_none_or(|block| block.2) => segment = index + 1,
			_ => {}
		}
	}
	blocks
}

/// Which palette a selector's variables belong to. The first `.theme-*` class decides, so
/// `:is(.theme-dark, .theme-light .theme-dark)` is dark. Darker variants are skipped.
fn scope(selector: &str) -> Option<Scope> {
	if let Some(start) = selector.find(".theme-") {
		let name: String = selector[start + 7..]
			.chars()
			.take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
			.collect();
		return match name.as_str() {
			"dark" => Some(Scope::Dark),
			"light" => Some(Scope::Light),
			_ => None,
		};
	}
	selector
		.split(',')
		.all(|part| {
			matches!(
				part.trim(),
				":root" | "html" | "body" | ".visual-refresh" | ":root .visual-refresh"
			)
		})
		.then_some(Scope::Root)
}

fn resolve(value: &str, scope: &BTreeMap<String, String>, depth: usize) -> Option<String> {
	if depth > MAX_VAR_DEPTH {
		return None;
	}
	let Some(start) = value.find("var(") else {
		return Some(value.to_owned());
	};
	let inner_start = start + 4;
	let mut level = 1;
	let end = value[inner_start..].char_indices().find_map(|(i, c)| {
		match c {
			'(' => level += 1,
			')' => level -= 1,
			_ => {}
		}
		(level == 0).then_some(inner_start + i)
	})?;
	let inner = &value[inner_start..end];
	let (name, fallback) = inner
		.split_once(',')
		.map_or((inner.trim(), None), |(name, fallback)| {
			(name.trim(), Some(fallback.trim()))
		});
	let replacement = match scope.get(name) {
		Some(value) => resolve(value, scope, depth + 1)?,
		None => resolve(fallback?, scope, depth + 1)?,
	};
	let resolved = format!("{}{replacement}{}", &value[..start], &value[end + 1..]);
	(resolved.len() <= MAX_VALUE_BYTES).then(|| resolve(&resolved, scope, depth + 1))?
}

fn color(scope: &BTreeMap<String, String>, name: &str) -> Option<Rgba> {
	parse_css_color(&resolve(scope.get(name)?, scope, 0)?)
}

/// `#rgb[a]`, `#rrggbb[aa]`, `rgb[a]()` and `hsl[a]()` in comma or space syntax.
fn parse_css_color(value: &str) -> Option<Rgba> {
	let value = value.trim().to_ascii_lowercase();
	if let Some(hex) = value.strip_prefix('#') {
		if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
			return None;
		}
		let hex: String = match hex.len() {
			3 | 4 => hex.chars().flat_map(|c| [c, c]).collect(),
			6 | 8 => hex.to_owned(),
			_ => return None,
		};
		let mut rgba = [255; 4];
		for (i, slot) in rgba.iter_mut().enumerate().take(hex.len() / 2) {
			*slot = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
		}
		return Some(rgba);
	}
	let (function, args) = value.split_once('(')?;
	let args = args.strip_suffix(')')?;
	let parts: Vec<&str> = args
		.split([',', ' ', '/'])
		.filter(|part| !part.is_empty())
		.collect();
	if !(3..=4).contains(&parts.len()) {
		return None;
	}
	let number = |part: &str, scale: f32| -> Option<f32> {
		let value = part.strip_suffix('%').map_or_else(
			|| part.trim_end_matches("deg").parse::<f32>().ok(),
			|percent| percent.parse::<f32>().ok().map(|p| p / 100.0 * scale),
		)?;
		value.is_finite().then_some(value)
	};
	let alpha = parts.get(3).map_or(Some(1.0), |part| number(part, 1.0))?;
	let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
	let rgb = match function {
		"rgb" | "rgba" => [
			number(parts[0], 255.0)? / 255.0,
			number(parts[1], 255.0)? / 255.0,
			number(parts[2], 255.0)? / 255.0,
		],
		"hsl" | "hsla" => hsl(
			number(parts[0], 360.0)?,
			number(parts[1], 1.0)?,
			number(parts[2], 1.0)?,
		),
		_ => return None,
	};
	Some([
		channel(rgb[0]),
		channel(rgb[1]),
		channel(rgb[2]),
		channel(alpha),
	])
}

/// Hue in degrees; saturation and lightness as fractions, or unitless 0–100 numbers.
fn hsl(hue: f32, saturation: f32, lightness: f32) -> [f32; 3] {
	let fraction = |value: f32| if value > 1.0 { value / 100.0 } else { value }.clamp(0.0, 1.0);
	let (s, l) = (fraction(saturation), fraction(lightness));
	let h = hue.rem_euclid(360.0) / 60.0;
	let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
	let x = c * (1.0 - (h % 2.0 - 1.0).abs());
	let (r, g, b) = match h as u8 {
		0 => (c, x, 0.0),
		1 => (x, c, 0.0),
		2 => (0.0, c, x),
		3 => (0.0, x, c),
		4 => (x, 0.0, c),
		_ => (c, 0.0, x),
	};
	let m = l - c / 2.0;
	[r + m, g + m, b + m]
}

fn hex(color: Rgba) -> String {
	format!(
		"#{:02X}{:02X}{:02X}{:02X}",
		color[0], color[1], color[2], color[3]
	)
}

fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
	let channel = |i: usize| (f32::from(a[i]) * (1.0 - t) + f32::from(b[i]) * t).round() as u8;
	[channel(0), channel(1), channel(2), 255]
}

fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
	[
		color[0],
		color[1],
		color[2],
		(alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
	]
}

fn luminance(color: Rgba) -> f32 {
	0.2126 * f32::from(color[0]) + 0.7152 * f32::from(color[1]) + 0.0722 * f32::from(color[2])
}

/// The native palette and how many of its colors came from the CSS.
fn palette(scope: &BTreeMap<String, String>, dark: bool) -> (ThemePalette, usize) {
	let mut colors = BTreeMap::new();
	for (key, names) in COLORS {
		if let Some(value) = names.iter().find_map(|name| color(scope, name)) {
			colors.insert(*key, value);
		}
	}
	let mapped = colors.len();
	let (white, black) = ([255, 255, 255, 255], [0, 0, 0, 255]);
	let toward = if dark { white } else { black };
	let mut background = None;
	// ClearVision shades one window image with translucent layers instead of solid surfaces:
	// every panel has the app layer, side panels add a card layer, and the conversation and
	// header add a second app layer. Coverage composes as 1 - product(1 - alpha).
	if let Some(shading) = color(scope, "--background-shading") {
		let card = color(scope, "--card-shading").unwrap_or(shading);
		let ink = with_alpha(shading, 1.0);
		for key in ["base", "sidebar", "chat"] {
			colors.entry(key).or_insert(ink);
		}
		colors
			.entry("raised")
			.or_insert(with_alpha(mix(ink, toward, 0.08), 0.85));
		colors
			.entry("hover")
			.or_insert(with_alpha(mix(ink, toward, 0.5), 0.12));
		colors
			.entry("border")
			.or_insert(with_alpha(mix(ink, toward, 0.5), 0.18));
		let (s, c) = (f32::from(shading[3]) / 255.0, f32::from(card[3]) / 255.0);
		let cover = |layers: &[f32]| {
			((1.0 - layers.iter().map(|a| 1.0 - a).product::<f32>()) * 100.0).round() as u8
		};
		background = Some(Background {
			opacity: 100,
			fit: BackgroundFit::Cover,
			target: BackgroundTarget::Window,
			sections: Some(SectionOpacity {
				top_bar: cover(&[s, s, c]),
				server_list: cover(&[s]),
				channel_list: cover(&[s, c]),
				message_list: cover(&[s, s]),
				member_list: cover(&[s, c]),
				composer: cover(&[s, s]),
			}),
		});
	}
	if let Some(text) = colors.get("text").copied() {
		colors
			.entry("text_strong")
			.or_insert(mix(text, toward, 0.5));
	}
	if let Some(accent) = colors.get("accent").copied() {
		let accent = with_alpha(accent, 1.0);
		colors.insert(
			"accent_text",
			if luminance(accent) > 190.0 {
				black
			} else {
				white
			},
		);
		colors
			.entry("mention_bg")
			.or_insert(with_alpha(accent, 0.25));
		colors
			.entry("mention_text")
			.or_insert(mix(accent, toward, 0.35));
	}
	(
		ThemePalette {
			background,
			colors: colors
				.into_iter()
				.map(|(key, value)| (key.to_owned(), hex(value)))
				.collect(),
			backdrop: None,
		},
		mapped,
	)
}

fn font_families(scope: &BTreeMap<String, String>) -> Vec<String> {
	let Some(value) = FONT_VARS
		.iter()
		.find_map(|name| resolve(scope.get(*name)?, scope, 0))
	else {
		return Vec::new();
	};
	value
		.split(',')
		.map(|family| family.trim().trim_matches(['"', '\'']).trim())
		.filter(|family| {
			!family.is_empty()
				&& family.len() <= 128
				&& !family.chars().any(char::is_control)
				&& !GENERIC_FONTS.contains(&family.to_ascii_lowercase().as_str())
				&& !family.starts_with("var(")
		})
		.take(MAX_FONT_FAMILIES)
		.map(str::to_owned)
		.collect()
}

fn background_url(scope: &BTreeMap<String, String>) -> Option<String> {
	IMAGE_VARS.iter().find_map(|name| {
		let value = resolve(scope.get(*name)?, scope, 0)?;
		let start = value.find("url(")? + 4;
		let end = value[start..].find(')')? + start;
		let url = value[start..end].trim().trim_matches(['"', '\'']);
		valid_https_url(url).then(|| url.to_owned())
	})
}

/// BetterDiscord-style `@name`/`@author`/`@version`/`@source`/`@license` header values,
/// keeping only valid ones.
fn metadata(css: &str, manifest: &mut Manifest) {
	let Some(header) = css
		.trim_start()
		.strip_prefix("/*")
		.and_then(|rest| rest.split_once("*/"))
		.map(|(header, _)| header)
	else {
		return;
	};
	for line in header.lines() {
		let line = line.trim().trim_start_matches('*').trim();
		let Some((key, value)) = line.strip_prefix('@').and_then(|line| line.split_once(' '))
		else {
			continue;
		};
		let value = value.trim();
		if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
			continue;
		}
		match key {
			"name" => manifest.name = value.into(),
			"author" => manifest.author = value.into(),
			"version" => manifest.version = value.into(),
			"license" => manifest.license = value.into(),
			"source" if valid_https_url(value) => manifest.source = value.into(),
			_ => {}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const CLEARVISION: &str = r#"/**
 * @name Winter Sample
 * @author Synthetic Author
 * @version 7.0.1
 * @source https://example.org/theme
 */
@import url("https://example.org/main.css");
:root {
  --main-color: #66aacc; /* accent */
  --success-color: #43b581;
  --danger-color: #982929;
  --idle-color: #faa61a;
  --background-image: url(https://example.org/winter.jpg);
  --main-font: "gg sans", "Helvetica Neue", sans-serif;
  --channel-selected-bg: var(--main-color);
}
:is(.theme-light, .theme-dark .theme-light) {
  --background-shading: rgba(252, 252, 252, 0.3);
  --card-shading: rgba(252, 252, 252, 0.3);
  --normal-text: #36363c;
}
:is(.theme-dark, .theme-light .theme-dark) {
  --background-shading: rgba(0, 0, 0, 0.4);
  --card-shading: rgba(0, 0, 0, 0.2);
  --normal-text: #d8d8db;
}
:is(.theme-darker, .theme-light .theme-darker) { --normal-text: #fbfbfb; }
.chat_abc { color: red; }
"#;

	#[test]
	fn clearvision_settings_become_a_shaded_window_theme() {
		let import = import_css(CLEARVISION, "imported".into()).unwrap();
		let theme = import.package.theme.as_ref().unwrap();
		let dark = &theme.dark.colors;
		assert_eq!(dark["accent"], "#66AACCFF");
		assert_eq!(dark["selected"], "#66AACCFF", "resolved through var()");
		// The exact dark scope wins over the darker variant; light keeps its own text.
		assert_eq!(dark["text"], "#D8D8DBFF");
		assert_eq!(theme.light.colors["text"], "#36363CFF");
		assert_eq!(dark["chat"], "#000000FF");
		let sections = theme.dark.background.unwrap().sections.unwrap();
		assert_eq!(
			[
				sections.server_list,
				sections.channel_list,
				sections.message_list,
				sections.top_bar
			],
			[40, 52, 64, 71]
		);
		assert_eq!(
			import.background_url.as_deref(),
			Some("https://example.org/winter.jpg")
		);
		assert_eq!(import.fonts, ["gg sans", "Helvetica Neue"]);
		// The @import, the darker variant and the DOM rule are reported, not applied.
		assert_eq!(import.ignored, 3);
		let manifest = &import.package.manifest;
		assert_eq!(
			(manifest.name.as_str(), manifest.version.as_str()),
			("Winter Sample", "7.0.1")
		);
		assert_eq!(manifest.source, "https://example.org/theme");
		assert_eq!(manifest.license, "NOASSERTION");
	}

	#[test]
	fn discord_token_themes_map_space_and_hsl_colors() {
		let css = ".theme-dark { --background-primary: #1e1e2e; \
			--text-normal: rgb(205 214 244 / 100%); --brand-experiment: hsl(267 84% 81%); }";
		let import = import_css(css, "tokens".into()).unwrap();
		let theme = import.package.theme.unwrap();
		assert_eq!(theme.dark.colors["chat"], "#1E1E2EFF");
		assert_eq!(theme.dark.colors["text"], "#CDD6F4FF");
		assert_eq!(theme.dark.colors["accent"], "#CAA6F7FF");
		assert!(theme.dark.background.is_none());
		assert!(theme.light.colors.is_empty());
	}

	#[test]
	fn malformed_or_unrelated_css_is_rejected_without_hanging() {
		let cyclic = ":root { --a: var(--b); --b: var(--a); --main-color: var(--a); \
			--text-normal: var(--missing, #fff) } .x { color: red";
		let import = import_css(cyclic, "cyclic".into()).unwrap();
		let dark = &import.package.theme.unwrap().dark.colors;
		assert!(!dark.contains_key("accent"));
		assert_eq!(dark["text"], "#FFFFFFFF", "var() fallback");
		assert!(matches!(
			import_css(".chat { color: red; } }}{", "x".into()),
			Err(Error::Invalid)
		));
		let doubling = format!(
			":root {{ --a0: #fff; {} --text-normal: var(--a20); }}",
			(1..=20)
				.map(|i| format!("--a{i}: var(--a{0}) var(--a{0});", i - 1))
				.collect::<String>()
		);
		assert!(import_css(&doubling, "x".into()).is_err());
		assert!(matches!(
			import_css(&" ".repeat(MAX_CSS_BYTES + 1), "x".into()),
			Err(Error::Limit)
		));
	}
}
