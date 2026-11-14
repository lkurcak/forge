//! `forge web`: each repo's deployed site, checked from outside, the way a
//! browser or phone meets it. The page needs a title, a viewport for phones,
//! a theme color, an icon, an icon for iOS's home screen, and a web app
//! manifest that browsers can install it from. Its health check has to
//! answer without a login, and should say which version is deployed. It only
//! reports: fixing any of it means changing the app.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Cursor;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Result, bail};
use html5gum::{DefaultEmitter, Token, Tokenizer};
use serde_json::Value;
use ureq::ResponseExt as _;

use crate::manifest::{Repo, Site};
use crate::progress::Progress;

/// How long one request may take.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The `display` modes that open an installed app in a window of its own.
const APP_DISPLAYS: [&str; 3] = ["standalone", "fullscreen", "minimal-ui"];

/// The home screen icon's size on iPhones; iOS scales larger ones down.
const APPLE_ICON: u32 = 180;

/// The members a web app manifest needs to install as an app.
const MANIFEST_MEMBERS: [&str; 6] = [
    "name",
    "short_name",
    "start_url",
    "display",
    "theme_color",
    "background_color",
];

/// Checks the site of each of `repos` that has one. With `named`, the repos
/// were named on the command line, so each must have a site. Fails when any
/// check does.
pub fn run(repos: &[&Repo], named: bool) -> Result<ExitCode> {
    if named && let Some(repo) = repos.iter().find(|r| r.site.is_none()) {
        bail!("{} has no url in the manifest", repo.name);
    }
    let sites: Vec<(&str, &Site)> = repos
        .iter()
        .filter_map(|r| r.site.as_ref().map(|site| (r.name.as_str(), site)))
        .collect();
    if sites.is_empty() {
        bail!("no repo in the manifest has a url: give each deployed one the address of its page");
    }

    let http = Http::new();
    let progress = Progress::new(&sites.iter().map(|(name, _)| *name).collect::<Vec<_>>());
    let mut failed = false;
    crate::in_parallel(
        &sites,
        |i, (_, site)| {
            progress.start(i);
            let report = check(&http, site);
            progress.finish(i, report.mark());
            report
        },
        |i, report| {
            progress.report(i, &report.render(sites[i].0));
            failed |= !report.failures.is_empty();
        },
    );
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// A response, read whole.
#[derive(Clone, Debug, Default)]
pub struct Fetched {
    pub status: u16,
    /// The media type, lowercase and without parameters; empty if not given.
    pub content_type: String,
    /// Where it came from, after any redirects.
    pub url: String,
    /// Where a redirect that wasn't followed leads.
    pub location: Option<String>,
    pub body: Vec<u8>,
}

/// Something that GETs URLs: the network, or canned responses in tests.
pub trait Fetch: Sync {
    /// GETs `url`, following redirects if `follow`.
    fn get(&self, url: &str, follow: bool) -> Result<Fetched>;
}

/// The network, without cookies: each request goes out as a stranger's
/// would.
struct Http(ureq::Agent);

impl Http {
    fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .user_agent(concat!("forge/", env!("CARGO_PKG_VERSION")))
            .build();
        Self(config.into())
    }
}

impl Fetch for Http {
    fn get(&self, url: &str, follow: bool) -> Result<Fetched> {
        let request = self.0.get(url);
        let mut response = if follow {
            request.call()?
        } else {
            request.config().max_redirects(0).build().call()?
        };
        let header = |name| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let content_type = header("content-type").unwrap_or_default();
        let location = header("location");
        Ok(Fetched {
            status: response.status().as_u16(),
            content_type: media_type(&content_type),
            url: response.get_uri().to_string(),
            location,
            body: response.body_mut().read_to_vec()?,
        })
    }
}

/// `text/html; charset=utf-8` as `text/html`.
fn media_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// What `forge web` found wrong with a site.
#[derive(Debug, Default)]
struct Report {
    /// What fails a check.
    failures: Vec<String>,
    /// Worth fixing, but not failing.
    warnings: Vec<String>,
    /// What the health check says is deployed: its version, and its commit
    /// if given.
    deployed: Option<String>,
}

impl Report {
    const fn mark(&self) -> &'static str {
        if self.failures.is_empty() {
            "✓"
        } else {
            "✗"
        }
    }

    /// The mark, the repo's name and what's deployed, then a line per finding.
    fn render(&self, name: &str) -> String {
        let mut text = format!("{} {name}", self.mark());
        let mut add =
            |line: std::fmt::Arguments| write!(text, "{line}").expect("writing to a String");
        if let Some(deployed) = &self.deployed {
            add(format_args!(" {deployed}"));
        }
        for line in &self.failures {
            add(format_args!("\n    {line}"));
        }
        for line in &self.warnings {
            add(format_args!("\n    warning: {line}"));
        }
        text
    }
}

fn check(fetch: &impl Fetch, site: &Site) -> Report {
    let mut checker = Checker {
        fetch,
        origin: origin(&site.url),
        report: Report::default(),
    };
    checker.page(&site.url);
    if let Some(health) = &site.health {
        checker.health(health);
    }
    checker.report
}

struct Checker<'a, F> {
    fetch: &'a F,
    /// The page's scheme and host, left off URLs under it when shown.
    origin: String,
    report: Report,
}

impl<F: Fetch> Checker<'_, F> {
    fn fail(&mut self, line: String) {
        self.report.failures.push(line);
    }

    fn warn(&mut self, line: String) {
        self.report.warnings.push(line);
    }

    /// `url` as shown: just its path, if it's on the page's host.
    fn short(&self, url: &str) -> String {
        url.strip_prefix(&self.origin)
            .filter(|path| path.starts_with('/'))
            .unwrap_or(url)
            .to_owned()
    }

    /// Fetches the image at `url`, or says why it isn't one, as in
    /// "is HTTP 404 Not Found".
    fn image(&self, url: &str) -> Result<Fetched, String> {
        let fetched = self
            .fetch
            .get(url, true)
            .map_err(|err| format!("failed: {err:#}"))?;
        if fetched.status != 200 {
            return Err(format!("is {}", status(fetched.status)));
        }
        if !fetched.content_type.starts_with("image/") {
            return Err(served_as(&fetched.content_type, "an image"));
        }
        Ok(fetched)
    }

    fn page(&mut self, url: &str) {
        let page = match self.fetch.get(url, true) {
            Ok(page) => page,
            Err(err) => return self.fail(format!("the page failed: {err:#}")),
        };
        if page.status != 200 {
            return self.fail(format!("the page is {}", status(page.status)));
        }
        if page.content_type != "text/html" {
            return self.fail(format!(
                "the page {}",
                served_as(&page.content_type, "HTML")
            ));
        }
        let head = Head::parse(&String::from_utf8_lossy(&page.body));
        // A <base> moves what the page's links are relative to.
        let base = head
            .base
            .as_deref()
            .and_then(|href| resolve(&page.url, href))
            .unwrap_or(page.url);

        match head.title.as_deref().map(str::trim) {
            None => self.fail("no <title>".into()),
            Some("") => self.fail("an empty <title>".into()),
            Some(_) => {}
        }
        match head.meta("viewport") {
            None => self.fail(
                "no <meta name=\"viewport\">, so phones show it zoomed out, as a desktop page"
                    .into(),
            ),
            Some(content)
                if !content
                    .replace(char::is_whitespace, "")
                    .to_ascii_lowercase()
                    .contains("width=device-width") =>
            {
                self.fail(format!(
                    "<meta name=\"viewport\"> lacks width=device-width (it's {content:?}), so \
                     phones show it zoomed out"
                ));
            }
            Some(_) => {}
        }
        if head.meta("theme-color").is_none_or(|c| c.trim().is_empty()) {
            self.fail(
                "no <meta name=\"theme-color\">, the color of the browser's bar around it".into(),
            );
        }

        self.icons(&base, &head);
        self.apple_icon(&base, &head);
        self.manifest(&base, &head);
    }

    /// The tab's icon: every `<link rel="icon">`, else `/favicon.ico`.
    fn icons(&mut self, base: &str, head: &Head) {
        let hrefs: Vec<&str> = head.links("icon").collect();
        if hrefs.is_empty() {
            let url = resolve(base, "/favicon.ico").expect("an absolute path resolves");
            if let Err(why) = self.image(&url) {
                self.fail(format!(
                    "no icon: no <link rel=\"icon\">, and /favicon.ico {why}"
                ));
            }
            return;
        }
        for href in hrefs {
            let Some(url) = self.resolve_link(base, href, "icon") else {
                continue;
            };
            if let Err(why) = self.image(&url) {
                let shown = self.short(&url);
                self.fail(format!("icon {shown} {why}"));
            }
        }
    }

    /// The icon iOS puts on the home screen: `<link rel="apple-touch-icon">`,
    /// else `/apple-touch-icon.png`. It must be a square PNG at least 180
    /// pixels wide, and opaque, since iOS fills transparent pixels with black.
    fn apple_icon(&mut self, base: &str, head: &Head) {
        let href = head
            .links("apple-touch-icon")
            .chain(head.links("apple-touch-icon-precomposed"))
            .next();
        let url = match href {
            Some(href) => match self.resolve_link(base, href, "apple-touch-icon") {
                Some(url) => url,
                None => return,
            },
            None => resolve(base, "/apple-touch-icon.png").expect("an absolute path resolves"),
        };
        let shown = self.short(&url);
        let image = match self.image(&url) {
            Ok(image) => image,
            Err(why) if href.is_none() => {
                return self.fail(format!(
                    "no apple-touch-icon: no <link rel=\"apple-touch-icon\">, and \
                     /apple-touch-icon.png {why}, so iOS puts a screenshot on the home screen"
                ));
            }
            Err(why) => return self.fail(format!("apple-touch-icon {shown} {why}")),
        };
        let Some((width, height)) = png_size(&image.body) else {
            return self.fail(format!(
                "apple-touch-icon {shown} isn't a PNG, which is all iOS takes"
            ));
        };
        if width != height || width < APPLE_ICON {
            self.fail(format!(
                "apple-touch-icon {shown} is {width}x{height}: make it {APPLE_ICON}x{APPLE_ICON}"
            ));
        }
        match transparent(&image.body) {
            Ok(false) => {}
            Ok(true) => self.warn(format!(
                "apple-touch-icon {shown} has transparent pixels, which iOS shows as black"
            )),
            Err(err) => self.fail(format!("apple-touch-icon {shown} can't be decoded: {err}")),
        }
    }

    /// The web app manifest, which makes the site installable as an app.
    fn manifest(&mut self, base: &str, head: &Head) {
        let Some(href) = head.links("manifest").next() else {
            return self
                .fail("no <link rel=\"manifest\">, so it can't be installed as an app".into());
        };
        let Some(url) = self.resolve_link(base, href, "manifest") else {
            return;
        };
        let shown = format!("manifest {}", self.short(&url));
        let fetched = match self.fetch.get(&url, true) {
            Ok(fetched) => fetched,
            Err(err) => return self.fail(format!("{shown} failed: {err:#}")),
        };
        if fetched.status != 200 {
            return self.fail(format!("{shown} is {}", status(fetched.status)));
        }
        let manifest = match serde_json::from_slice::<Value>(&fetched.body) {
            Ok(manifest @ Value::Object(_)) => manifest,
            Ok(_) => return self.fail(format!("{shown} isn't a JSON object")),
            Err(err) => {
                let served = served_as(&fetched.content_type, "JSON");
                return self.fail(format!("{shown} {served}: {err}"));
            }
        };
        if !matches!(
            fetched.content_type.as_str(),
            "application/manifest+json" | "application/json"
        ) {
            let served = served_as(&fetched.content_type, "application/manifest+json");
            self.warn(format!("{shown} {served}"));
        }

        let missing: Vec<&str> = MANIFEST_MEMBERS
            .into_iter()
            .filter(|key| manifest[key].as_str().is_none_or(|v| v.trim().is_empty()))
            .collect();
        if !missing.is_empty() {
            self.fail(format!("{shown} lacks {}", missing.join(", ")));
        }
        if let Some(display) = manifest["display"].as_str()
            && !display.trim().is_empty()
            && !APP_DISPLAYS.contains(&display)
        {
            self.fail(format!(
                "{shown} has display {display:?}, so installed it opens as a browser tab: \
                 make it standalone"
            ));
        }
        self.manifest_icons(&url, &shown, &manifest["icons"]);
    }

    /// The manifest's icons: a 192x192 one, a 512x512 one, a maskable one
    /// (Android crops others into its own shape), each there and the size it
    /// says.
    fn manifest_icons(&mut self, manifest: &str, shown: &str, icons: &Value) {
        let none = Vec::new();
        let icons = icons.as_array().unwrap_or(&none);
        let mut lacking = Vec::new();
        for size in ["192x192", "512x512"] {
            if !icons
                .iter()
                .any(|icon| tokens(&icon["sizes"]).any(|s| s == size))
            {
                lacking.push(size);
            }
        }
        if !icons
            .iter()
            .any(|icon| tokens(&icon["purpose"]).any(|p| p == "maskable"))
        {
            lacking.push("maskable");
        }
        if !lacking.is_empty() {
            self.fail(format!("{shown} has no {} icon", join_or(&lacking)));
        }

        // An image listed twice, for two purposes say, is fetched once.
        let mut fetched: BTreeMap<String, Result<Option<(u32, u32)>, String>> = BTreeMap::new();
        for icon in icons {
            let Some(src) = icon["src"].as_str() else {
                self.fail(format!("{shown} has an icon without src"));
                continue;
            };
            let Some(url) = self.resolve_link(manifest, src, "manifest icon") else {
                continue;
            };
            let image = fetched
                .entry(url.clone())
                .or_insert_with(|| self.image(&url).map(|image| png_size(&image.body)));
            let icon_shown = self.short(&url);
            match image {
                Err(why) => {
                    let line = format!("manifest icon {icon_shown} {why}");
                    if !self.report.failures.contains(&line) {
                        self.fail(line);
                    }
                }
                Ok(Some((width, height))) => {
                    let declared: Vec<String> =
                        tokens(&icon["sizes"]).filter(|s| s != "any").collect();
                    let actual = format!("{width}x{height}");
                    if !declared.is_empty() && !declared.contains(&actual) {
                        self.fail(format!(
                            "manifest icon {icon_shown} is {actual}, not {} as the manifest says",
                            declared.join(" or ")
                        ));
                    }
                }
                Ok(None) => {}
            }
        }
    }

    /// `href` from the page (or manifest) at `base` as an absolute URL, or
    /// `None` when there's nothing to fetch: a `data:` URL is there already,
    /// and one that isn't http(s) is reported.
    fn resolve_link(&mut self, base: &str, href: &str, what: &str) -> Option<String> {
        if href.trim_start().starts_with("data:") {
            return None;
        }
        let url = resolve(base, href);
        if url.is_none() {
            self.fail(format!("{what} {href} isn't an http(s) URL"));
        }
        url
    }

    /// The health check answers 2xx to anyone, with no login and no
    /// redirect, and should say what version is deployed.
    fn health(&mut self, url: &str) {
        let shown = format!("health {}", self.short(url));
        let fetched = match self.fetch.get(url, false) {
            Ok(fetched) => fetched,
            Err(err) => return self.fail(format!("{shown} failed: {err:#}")),
        };
        if (300..400).contains(&fetched.status) {
            let to = fetched
                .location
                .map_or_else(String::new, |to| format!(" to {}", self.short(&to)));
            return self.fail(format!(
                "{shown} redirects{to}: it should answer anyone, without a login"
            ));
        }
        if !(200..300).contains(&fetched.status) {
            return self.fail(format!("{shown} is {}", status(fetched.status)));
        }
        // Single-page apps answer any path with their page.
        if fetched.content_type == "text/html" {
            return self.fail(format!(
                "{shown} answers with an HTML page, likely the app's own for any path, so it \
                 checks nothing"
            ));
        }
        self.report.deployed = deployed(&fetched.body);
        if self.report.deployed.is_none() {
            self.warn(format!(
                "{shown} doesn't say what's deployed: answer JSON with a \"version\""
            ));
        }
    }
}

/// The version a health check's JSON names, and its commit if it names one.
fn deployed(body: &[u8]) -> Option<String> {
    let json: Value = serde_json::from_slice(body).ok()?;
    let version = json["version"].as_str().filter(|v| !v.trim().is_empty())?;
    Some(
        match json["commit"].as_str().filter(|c| !c.trim().is_empty()) {
            Some(commit) if commit.len() > 12 && commit.bytes().all(|b| b.is_ascii_hexdigit()) => {
                format!("{version} ({})", &commit[..7])
            }
            Some(commit) => format!("{version} ({commit})"),
            None => version.to_owned(),
        },
    )
}

/// "HTTP 404 Not Found".
fn status(code: u16) -> String {
    let reason = ureq::http::StatusCode::from_u16(code)
        .ok()
        .and_then(|code| code.canonical_reason());
    reason.map_or_else(|| format!("HTTP {code}"), |r| format!("HTTP {code} {r}"))
}

/// Why a response of `content_type` isn't `wanted`: "is text/html, not an
/// image".
fn served_as(content_type: &str, wanted: &str) -> String {
    if content_type.is_empty() {
        format!("has no Content-Type, so it isn't {wanted}")
    } else {
        format!("is {content_type}, not {wanted}")
    }
}

/// A space-separated list in a manifest, like an icon's `sizes`, lowercase.
fn tokens(value: &Value) -> impl Iterator<Item = String> + '_ {
    value
        .as_str()
        .unwrap_or_default()
        .split_ascii_whitespace()
        .map(str::to_ascii_lowercase)
}

/// "a", "a or b", "a, b or c".
fn join_or(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// What forge reads from a page: its `<head>`, up to `<body>`.
#[derive(Debug, Default)]
struct Head {
    title: Option<String>,
    /// `<base href>`.
    base: Option<String>,
    /// Each `<meta name>`, lowercase, with its content.
    meta: Vec<(String, String)>,
    /// Each `<link>` with an href: its rel's words, lowercase, and the href.
    links: Vec<(Vec<String>, String)>,
}

impl Head {
    fn parse(html: &str) -> Self {
        let mut emitter = DefaultEmitter::default();
        // Reads <script> and <style> as text, so tags in them aren't taken
        // for the page's own.
        emitter.naively_switch_states(true);
        let mut head = Self::default();
        let mut title: Option<String> = None;
        for token in Tokenizer::new_with_emitter(html, emitter).flatten() {
            match token {
                Token::StartTag(tag) => {
                    let attr = |name: &str| {
                        tag.attributes
                            .get(name.as_bytes())
                            .map(|value| String::from_utf8_lossy(&value.value).into_owned())
                    };
                    match tag.name.as_slice() {
                        // An <svg> in the body can have a <title> of its own.
                        b"body" => break,
                        b"title" if head.title.is_none() => title = Some(String::new()),
                        b"base" if head.base.is_none() => head.base = attr("href"),
                        b"meta" => {
                            if let (Some(name), Some(content)) = (attr("name"), attr("content")) {
                                head.meta.push((name.trim().to_ascii_lowercase(), content));
                            }
                        }
                        b"link" => {
                            if let (Some(rel), Some(href)) = (attr("rel"), attr("href")) {
                                let rel = rel
                                    .split_ascii_whitespace()
                                    .map(str::to_ascii_lowercase)
                                    .collect();
                                head.links.push((rel, href));
                            }
                        }
                        _ => {}
                    }
                }
                Token::String(text) => {
                    if let Some(title) = &mut title {
                        title.push_str(&String::from_utf8_lossy(&text.value));
                    }
                }
                Token::EndTag(tag) if tag.name.as_slice() == b"title" => {
                    if let Some(title) = title.take() {
                        head.title = Some(title);
                    }
                }
                _ => {}
            }
        }
        if head.title.is_none() {
            head.title = title;
        }
        head
    }

    /// The content of the first `<meta>` of this name.
    fn meta(&self, name: &str) -> Option<&str> {
        self.meta
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, content)| content.as_str())
    }

    /// The hrefs of the `<link>`s whose rel has this word.
    fn links<'a>(&'a self, rel: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.links
            .iter()
            .filter(move |(rels, _)| rels.iter().any(|r| r == rel))
            .map(|(_, href)| href.as_str())
    }
}

/// `href` resolved against `base`, an absolute http(s) URL, and without a
/// fragment; `None` if it leads somewhere other than http(s), like `data:`.
pub fn resolve(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    let href = href.split('#').next().unwrap_or_default();
    if let Some((scheme, _)) = href.split_once(':')
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        let scheme = scheme.to_ascii_lowercase();
        return (scheme == "http" || scheme == "https").then(|| href.to_owned());
    }

    let (scheme, rest) = base.split_once("://")?;
    let rest = rest.split('#').next().unwrap_or_default();
    let (authority, rest) = rest.split_at(rest.find(['/', '?']).unwrap_or(rest.len()));
    if let Some(network) = href.strip_prefix("//") {
        return Some(format!("{scheme}://{network}"));
    }
    let (base_path, base_query) = rest.split_at(rest.find('?').unwrap_or(rest.len()));
    let base_path = if base_path.is_empty() { "/" } else { base_path };
    let joined = if href.is_empty() {
        format!("{base_path}{base_query}")
    } else if href.starts_with('?') {
        format!("{base_path}{href}")
    } else if href.starts_with('/') {
        href.to_owned()
    } else {
        let dir = &base_path[..=base_path.rfind('/').unwrap_or(0)];
        format!("{dir}{href}")
    };
    let (path, query) = joined.split_at(joined.find('?').unwrap_or(joined.len()));
    Some(format!(
        "{scheme}://{authority}{}{query}",
        remove_dots(path)
    ))
}

/// A path without its `.` and `..` segments.
fn remove_dots(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    let mut kept: Vec<&str> = Vec::new();
    for (i, segment) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        match *segment {
            "." => {}
            ".." => {
                kept.pop();
            }
            segment => {
                kept.push(segment);
                continue;
            }
        }
        // `/a/..` and `/a/.` name a directory.
        if last {
            kept.push("");
        }
    }
    format!("/{}", kept.join("/"))
}

/// `https://example.com` for `https://example.com/app/`.
fn origin(url: &str) -> String {
    resolve(url, "/").map_or_else(String::new, |root| root.trim_end_matches('/').to_owned())
}

/// A PNG's width and height, from its header; `None` if it isn't a PNG.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || !bytes.starts_with(SIGNATURE) || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let number = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
    Some((number(16), number(20)))
}

/// Whether any pixel of a PNG is less than opaque.
fn transparent(bytes: &[u8]) -> Result<bool, png::DecodingError> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    // Palettes and tRNS chunks become an alpha channel, 8 bits deep.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let (color, _) = reader.output_color_type();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let frame = reader.next_frame(&mut buffer)?;
    let pixels = &buffer[..frame.buffer_size()];
    Ok(match color {
        png::ColorType::Rgba => pixels.as_chunks::<4>().0.iter().any(|p| p[3] < u8::MAX),
        png::ColorType::GrayscaleAlpha => pixels.as_chunks::<2>().0.iter().any(|p| p[1] < u8::MAX),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Canned responses by URL; anything else is a 404 page.
    #[derive(Default)]
    struct Fake(BTreeMap<String, Fetched>);

    impl Fake {
        fn with(mut self, url: &str, status: u16, content_type: &str, body: &[u8]) -> Self {
            self.0.insert(
                url.to_owned(),
                Fetched {
                    status,
                    content_type: media_type(content_type),
                    url: url.to_owned(),
                    location: None,
                    body: body.to_vec(),
                },
            );
            self
        }

        fn redirect(mut self, url: &str, to: &str) -> Self {
            self.0.insert(
                url.to_owned(),
                Fetched {
                    status: 302,
                    url: url.to_owned(),
                    location: Some(to.to_owned()),
                    ..Fetched::default()
                },
            );
            self
        }
    }

    impl Fetch for Fake {
        fn get(&self, url: &str, follow: bool) -> Result<Fetched> {
            let mut fetched = self.0.get(url).cloned().unwrap_or_else(|| Fetched {
                status: 404,
                content_type: "text/html".into(),
                url: url.to_owned(),
                ..Fetched::default()
            });
            if follow && let Some(to) = fetched.location.take() {
                return self.get(&resolve(url, &to).expect("a redirect to http(s)"), true);
            }
            Ok(fetched)
        }
    }

    fn png(size: u32, alpha: u8) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, size, size);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        let pixels = [10, 20, 30, alpha].repeat(usize::try_from(size * size).unwrap());
        writer.write_image_data(&pixels).unwrap();
        writer.finish().unwrap();
        out
    }

    const PAGE: &str = r##"<!doctype html>
        <html><head>
        <meta charset="utf-8">
        <meta name="viewport" content="width=device-width, initial-scale=1">
        <meta name="theme-color" content="#123456">
        <title>Deals</title>
        <link rel="icon" href="/favicon.svg" type="image/svg+xml">
        <link rel="apple-touch-icon" href="apple-touch-icon.png">
        <link rel="manifest" href="/manifest.webmanifest">
        </head><body><svg><title>chart</title></svg></body></html>"##;

    fn manifest() -> Value {
        json!({
            "name": "Deals",
            "short_name": "Deals",
            "start_url": "/",
            "display": "standalone",
            "theme_color": "#123456",
            "background_color": "#ffffff",
            "icons": [
                { "src": "icon-192.png", "sizes": "192x192", "type": "image/png" },
                { "src": "icon-512.png", "sizes": "512x512", "type": "image/png" },
                { "src": "icon-512.png", "sizes": "512x512", "purpose": "maskable" },
            ],
        })
    }

    /// A site that passes everything.
    fn good() -> Fake {
        Fake::default()
            .with("https://example.com/", 200, "text/html", PAGE.as_bytes())
            .with("https://example.com/favicon.svg", 200, "image/svg+xml", b"<svg/>")
            .with("https://example.com/apple-touch-icon.png", 200, "image/png", &png(180, 255))
            .with(
                "https://example.com/manifest.webmanifest",
                200,
                "application/manifest+json",
                manifest().to_string().as_bytes(),
            )
            .with("https://example.com/icon-192.png", 200, "image/png", &png(192, 0))
            .with("https://example.com/icon-512.png", 200, "image/png", &png(512, 0))
            .with(
                "https://example.com/healthz",
                200,
                "application/json",
                br#"{"status":"ok","version":"1.2.3","commit":"0123456789abcdef0123456789abcdef01234567"}"#,
            )
    }

    fn site(health: Option<&str>) -> Site {
        Site {
            url: "https://example.com/".into(),
            health: health.map(str::to_owned),
        }
    }

    #[test]
    fn a_good_site_passes() {
        let report = check(&good(), &site(Some("https://example.com/healthz")));
        assert_eq!(report.failures, Vec::<String>::new());
        assert_eq!(report.warnings, Vec::<String>::new());
        assert_eq!(report.deployed.as_deref(), Some("1.2.3 (0123456)"));
        assert_eq!(report.render("deals"), "✓ deals 1.2.3 (0123456)");
    }

    #[test]
    fn a_bare_page_fails_each_check() {
        let fake = Fake::default().with(
            "https://example.com/",
            200,
            "text/html; charset=utf-8",
            b"<html><head></head><body><h1>Hi</h1></body></html>",
        );
        let report = check(&fake, &site(None));
        assert_eq!(
            report.failures,
            [
                "no <title>",
                "no <meta name=\"viewport\">, so phones show it zoomed out, as a desktop page",
                "no <meta name=\"theme-color\">, the color of the browser's bar around it",
                "no icon: no <link rel=\"icon\">, and /favicon.ico is HTTP 404 Not Found",
                "no apple-touch-icon: no <link rel=\"apple-touch-icon\">, and \
                 /apple-touch-icon.png is HTTP 404 Not Found, so iOS puts a screenshot on the \
                 home screen",
                "no <link rel=\"manifest\">, so it can't be installed as an app",
            ]
        );
        assert_eq!(report.mark(), "✗");
    }

    #[test]
    fn fallback_icons_count_but_an_spa_fallback_does_not() {
        // Single-page apps often answer every path with their page.
        let page = "<title>x</title><meta name=viewport content=width=device-width>";
        let fake = Fake::default()
            .with("https://example.com/", 200, "text/html", page.as_bytes())
            .with(
                "https://example.com/favicon.ico",
                200,
                "image/x-icon",
                b"\0\0\x01\0",
            )
            .with(
                "https://example.com/apple-touch-icon.png",
                200,
                "text/html",
                page.as_bytes(),
            );
        let report = check(&fake, &site(None));
        assert!(
            !report.failures.iter().any(|f| f.contains("favicon")),
            "{report:?}"
        );
        assert!(
            report.failures.contains(
                &"no apple-touch-icon: no <link rel=\"apple-touch-icon\">, and \
                  /apple-touch-icon.png is text/html, not an image, so iOS puts a screenshot \
                  on the home screen"
                    .to_owned()
            ),
            "{report:?}"
        );
    }

    #[test]
    fn checks_the_icons_it_links() {
        let fake = good()
            .with("https://example.com/favicon.svg", 404, "text/html", b"")
            .with(
                "https://example.com/apple-touch-icon.png",
                200,
                "image/png",
                &png(120, 128),
            );
        let report = check(&fake, &site(None));
        assert_eq!(
            report.failures,
            [
                "icon /favicon.svg is HTTP 404 Not Found",
                "apple-touch-icon /apple-touch-icon.png is 120x120: make it 180x180",
            ]
        );
        assert_eq!(
            report.warnings,
            [
                "apple-touch-icon /apple-touch-icon.png has transparent pixels, which iOS shows as black"
            ]
        );
    }

    #[test]
    fn checks_the_manifest() {
        let mut manifest = manifest();
        manifest["display"] = json!("browser");
        manifest["short_name"] = json!("");
        manifest.as_object_mut().unwrap().remove("theme_color");
        manifest["icons"] = json!([
            { "src": "icon-192.png", "sizes": "192x192" },
            { "src": "missing.png", "sizes": "512x512" },
            { "src": "icon-512.png", "sizes": "256x256" },
        ]);
        let fake = good().with(
            "https://example.com/manifest.webmanifest",
            200,
            "application/octet-stream",
            manifest.to_string().as_bytes(),
        );
        let report = check(&fake, &site(None));
        assert_eq!(
            report.failures,
            [
                "manifest /manifest.webmanifest lacks short_name, theme_color",
                "manifest /manifest.webmanifest has display \"browser\", so installed it opens \
                 as a browser tab: make it standalone",
                "manifest /manifest.webmanifest has no maskable icon",
                "manifest icon /missing.png is HTTP 404 Not Found",
                "manifest icon /icon-512.png is 512x512, not 256x256 as the manifest says",
            ]
        );
        assert_eq!(
            report.warnings,
            [
                "manifest /manifest.webmanifest is application/octet-stream, not application/manifest+json"
            ]
        );
    }

    #[test]
    fn a_manifest_that_is_not_json_fails() {
        let fake = good().with(
            "https://example.com/manifest.webmanifest",
            200,
            "text/html",
            b"<!doctype html>",
        );
        let report = check(&fake, &site(None));
        assert_eq!(report.failures.len(), 1, "{report:?}");
        assert!(
            report.failures[0]
                .starts_with("manifest /manifest.webmanifest is text/html, not JSON: "),
            "{report:?}"
        );
    }

    #[test]
    fn manifest_icons_are_relative_to_the_manifest() {
        let page = PAGE.replace("/manifest.webmanifest", "/app/manifest.json");
        let fake = good()
            .with("https://example.com/", 200, "text/html", page.as_bytes())
            .with(
                "https://example.com/app/manifest.json",
                200,
                "application/json",
                manifest().to_string().as_bytes(),
            );
        let report = check(&fake, &site(None));
        assert_eq!(
            report.failures,
            [
                "manifest icon /app/icon-192.png is HTTP 404 Not Found",
                "manifest icon /app/icon-512.png is HTTP 404 Not Found",
            ]
        );
    }

    #[test]
    fn health_must_answer_without_a_login() {
        let health = "https://example.com/healthz";
        let redirected = good().redirect(health, "/login");
        let report = check(&redirected, &site(Some(health)));
        assert_eq!(
            report.failures,
            ["health /healthz redirects to /login: it should answer anyone, without a login"]
        );

        let page = good().with(health, 200, "text/html", PAGE.as_bytes());
        let report = check(&page, &site(Some(health)));
        assert_eq!(
            report.failures,
            [
                "health /healthz answers with an HTML page, likely the app's own for any path, \
                 so it checks nothing"
            ]
        );

        let down = good().with(health, 503, "text/plain", b"db down");
        let report = check(&down, &site(Some(health)));
        assert_eq!(
            report.failures,
            ["health /healthz is HTTP 503 Service Unavailable"]
        );

        // murmur answers 204, with no body to say what's deployed.
        let other_host = "https://api.example.org/health";
        let quiet = good().with(other_host, 204, "", b"");
        let report = check(&quiet, &site(Some(other_host)));
        assert_eq!(report.failures, Vec::<String>::new());
        assert_eq!(
            report.warnings,
            [
                "health https://api.example.org/health doesn't say what's deployed: answer JSON \
                 with a \"version\""
            ]
        );
    }

    #[test]
    fn reads_what_is_deployed() {
        assert_eq!(
            deployed(br#"{"version":"0.4.0"}"#).as_deref(),
            Some("0.4.0")
        );
        assert_eq!(
            deployed(br#"{"version":"0.4.0","commit":"dev"}"#).as_deref(),
            Some("0.4.0 (dev)")
        );
        assert_eq!(deployed(br#"{"status":"ok"}"#), None);
        assert_eq!(deployed(b"ok"), None);
    }

    #[test]
    fn parses_the_head() {
        let head = Head::parse(
            r##"<HTML><Head>
            <!-- <link rel="manifest" href="/old.json"> -->
            <script>document.write('<link rel="icon" href="/js.png">')</script>
            <title> A &amp; B </title>
            <base href="/app/">
            <META NAME="Theme-Color" content="#000">
            <link rel="shortcut icon" href="/a.ico?v=1&amp;x=2">
            <link rel=apple-touch-icon href=/touch.png>
            <link rel="stylesheet" href="/style.css">
            <link rel="icon">
            </head><body><svg><title>Not it</title></svg><link rel="manifest" href="/late.json"></body>"##,
        );
        assert_eq!(head.title.as_deref(), Some(" A & B "));
        assert_eq!(head.base.as_deref(), Some("/app/"));
        assert_eq!(head.meta("theme-color"), Some("#000"));
        assert_eq!(head.links("icon").collect::<Vec<_>>(), ["/a.ico?v=1&x=2"]);
        assert_eq!(
            head.links("apple-touch-icon").collect::<Vec<_>>(),
            ["/touch.png"]
        );
        assert_eq!(head.links("manifest").count(), 0);
    }

    #[test]
    fn resolves_links() {
        let base = "https://example.com/app/page.html?q=1#top";
        let cases = [
            ("icon.png", "https://example.com/app/icon.png"),
            ("./icon.png", "https://example.com/app/icon.png"),
            ("../icon.png", "https://example.com/icon.png"),
            ("../../../icon.png", "https://example.com/icon.png"),
            ("a/./b/../icon.png", "https://example.com/app/a/icon.png"),
            ("/icon.png?v=2#x", "https://example.com/icon.png?v=2"),
            ("..", "https://example.com/"),
            ("?v=3", "https://example.com/app/page.html?v=3"),
            ("", "https://example.com/app/page.html?q=1"),
            ("//cdn.example.org/i.png", "https://cdn.example.org/i.png"),
            (" HTTP://other.example/i.png ", "HTTP://other.example/i.png"),
        ];
        for (href, want) in cases {
            assert_eq!(resolve(base, href).as_deref(), Some(want), "{href}");
        }
        assert_eq!(resolve(base, "data:image/png;base64,AAAA"), None);
        assert_eq!(resolve(base, "mailto:me@example.com"), None);
        assert_eq!(
            resolve("https://example.com", "favicon.ico").as_deref(),
            Some("https://example.com/favicon.ico")
        );
        assert_eq!(origin("https://example.com/app/?x"), "https://example.com");
    }

    #[test]
    fn reads_pngs() {
        assert_eq!(png_size(&png(180, 255)), Some((180, 180)));
        assert_eq!(png_size(b"GIF89a, not a PNG at all"), None);
        assert!(!transparent(&png(4, 255)).unwrap());
        assert!(transparent(&png(4, 254)).unwrap());
        assert!(transparent(b"\x89PNG\r\n\x1a\n").is_err());
    }
}
