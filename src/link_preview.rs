use regex::Regex;
use std::{
    collections::VecDeque,
    io::Read,
    net::IpAddr,
    sync::LazyLock,
    time::{Duration, Instant},
};
use ureq::unversioned::{
    resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver},
    transport::{DefaultConnector, NextTimeout},
};
use url::Url;

#[derive(Clone)]
pub struct Preview {
    pub title: String,
    pub description: String,
    pub image: Option<crate::media::Decoded>,
    pub og: bool,
}

#[derive(Default)]
pub struct Cache(VecDeque<(String, Instant, Option<Preview>)>, u64);

impl Cache {
    pub fn get(&mut self, url: &str, epoch: u64) -> Option<Preview> {
        if self.1 != epoch {
            self.0.clear();
            self.1 = epoch;
        }
        self.0
            .retain(|(_, at, _)| at.elapsed() < Duration::from_secs(1800));
        if let Some((_, _, value)) = self.0.iter().find(|(key, _, _)| key == url) {
            return value.clone();
        }
        let value = fetch(url);
        self.0
            .push_back((url.to_owned(), Instant::now(), value.clone()));
        while self.0.len() > 64
            || self
                .0
                .iter()
                .filter_map(|(_, _, p)| p.as_ref()?.image.as_ref())
                .map(|i| i.rgba.len())
                .sum::<usize>()
                > 16 * 1024 * 1024
        {
            self.0.pop_front();
        }
        value
    }
}

pub fn first_url(text: &str) -> String {
    static LINKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>\"]+"#).unwrap());
    LINKS
        .find(text)
        .map(|v| {
            v.as_str()
                .trim_end_matches(['.', ',', ';', '!', ')', ']'])
                .to_owned()
        })
        .unwrap_or_default()
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 168 || b == 0 || (b == 88 && c == 99)))
                || (a == 192 && b == 0 && c == 2)
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            s[0] & 0xe000 == 0x2000
                && s[0] != 0x2001
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] <= 0x0fff)
        }
    }
}

#[derive(Debug)]
struct PublicResolver;

impl Resolver for PublicResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let addresses = DefaultResolver::default().resolve(uri, config, timeout)?;
        if addresses.iter().any(|addr| !public_ip(addr.ip())) {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(addresses)
    }
}

fn allowed(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(url.port_or_known_default(), Some(80 | 443))
        && url
            .host_str()
            .is_some_and(|host| host.contains('.') || host.parse::<IpAddr>().is_ok())
}

fn download(
    agent: &ureq::Agent,
    mut url: Url,
    limit: usize,
    deadline: Instant,
) -> Option<(Url, String, Vec<u8>)> {
    for _ in 0..=3 {
        if !allowed(&url) {
            return None;
        }
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let mut response = agent
            .get(url.as_str())
            .config()
            .timeout_global(Some(remaining.min(Duration::from_secs(5))))
            .build()
            .call()
            .ok()?;
        if response.status().is_redirection() {
            url = url
                .join(response.headers().get("location")?.to_str().ok()?)
                .ok()?;
            continue;
        }
        if !response.status().is_success() {
            return None;
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > limit {
            return None;
        }
        return Some((url, mime, bytes));
    }
    None
}

fn entities(text: &str, limit: usize) -> String {
    static ENTITY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"&(#x[0-9a-fA-F]+|#[0-9]+|amp|quot|apos|lt|gt|nbsp);").unwrap()
    });
    let decoded = ENTITY.replace_all(text, |caps: &regex::Captures<'_>| match &caps[1] {
        "amp" => "&".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "nbsp" => " ".into(),
        n => n
            .strip_prefix("#x")
            .and_then(|v| u32::from_str_radix(v, 16).ok())
            .or_else(|| n.strip_prefix('#')?.parse().ok())
            .and_then(char::from_u32)
            .map(|c| c.to_string())
            .unwrap_or_default(),
    });
    decoded
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect()
}

fn attributes(tag: &str) -> std::collections::HashMap<String, String> {
    static ATTR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?is)([\w:-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#).unwrap()
    });
    ATTR.captures_iter(tag)
        .map(|c| {
            (
                c[1].to_ascii_lowercase(),
                entities(
                    c.get(2)
                        .or_else(|| c.get(3))
                        .or_else(|| c.get(4))
                        .unwrap()
                        .as_str(),
                    4096,
                ),
            )
        })
        .collect()
}

fn metadata(html: &str) -> (String, String, String, String) {
    static TAG: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?is)<(?:meta|link)\b(?:[^>"']|"[^"]*"|'[^']*')*>"#).unwrap()
    });
    static TITLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<title\b[^>]*>(.*?)</title\s*>").unwrap());
    let mut values = std::collections::HashMap::new();
    let mut icon = String::new();
    for tag in TAG.find_iter(html).take(512) {
        let attrs = attributes(tag.as_str());
        if let (Some(key), Some(value)) = (
            attrs.get("property").or_else(|| attrs.get("name")),
            attrs.get("content"),
        ) {
            values
                .entry(key.to_ascii_lowercase())
                .or_insert_with(|| value.clone());
        }
        if icon.is_empty()
            && attrs
                .get("rel")
                .is_some_and(|r| r.split_whitespace().any(|v| v.eq_ignore_ascii_case("icon")))
        {
            icon = attrs.get("href").cloned().unwrap_or_default();
        }
    }
    let title = values
        .get("og:title")
        .cloned()
        .or_else(|| TITLE.captures(html).map(|c| entities(&c[1], 512)))
        .unwrap_or_default();
    let desc = values
        .get("og:description")
        .or_else(|| values.get("description"))
        .cloned()
        .unwrap_or_default();
    (
        entities(&title, 512),
        entities(&desc, 1024),
        values
            .get("og:image")
            .or_else(|| values.get("og:image:url"))
            .cloned()
            .unwrap_or_default(),
        icon,
    )
}

fn fetch(input: &str) -> Option<Preview> {
    let config = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(5)))
        .build();
    let agent = ureq::Agent::with_parts(config, DefaultConnector::default(), PublicResolver);
    let deadline = Instant::now() + Duration::from_secs(12);
    let (url, mime, bytes) = download(&agent, Url::parse(input).ok()?, 512 * 1024, deadline)?;
    if !mime.starts_with("text/html") && !mime.starts_with("application/xhtml+xml") {
        return None;
    }
    let (title, description, og_url, icon_url) = metadata(&String::from_utf8_lossy(&bytes));
    let load = |source: &str, size| {
        let (_, _, bytes) = download(&agent, url.join(source).ok()?, 2 * 1024 * 1024, deadline)?;
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(32 * 1024 * 1024);
        reader.limits(limits);
        let image = reader.decode().ok()?.thumbnail(size, size).to_rgba8();
        Some(crate::media::Decoded {
            w: image.width(),
            h: image.height(),
            rgba: image.into_raw(),
        })
    };
    let mut image = if og_url.is_empty() {
        None
    } else {
        load(&og_url, 600)
    };
    let og = image.is_some();
    if image.is_none() && !icon_url.is_empty() {
        image = load(&icon_url, 64);
    }
    if image.is_none() {
        image = load("/favicon.ico", 64);
    }
    Some(Preview {
        title,
        description,
        image,
        og,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_non_public_addresses() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "100.100.100.200",
            "169.254.169.254",
            "192.168.1.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "2002:7f00:1::",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
        assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn rejects_credentials_schemes_and_ports() {
        for value in [
            "file:///secret",
            "http://user:pass@example.com",
            "http://example.com:8080",
            "http://localhost",
        ] {
            assert!(!allowed(&Url::parse(value).unwrap()));
        }
    }

    #[test]
    fn parses_metadata_and_favicon_independently() {
        let (title, desc, image, icon) = metadata(
            "<TITLE>Fallback</TITLE><meta content='Big &amp; nice' property='og:title'><meta name=description content='Details'><link href='/small.ico' rel='shortcut icon'><meta property='og:image' content='/large.jpg'>",
        );
        assert_eq!(
            (title.as_str(), desc.as_str(), image.as_str(), icon.as_str()),
            ("Big & nice", "Details", "/large.jpg", "/small.ico")
        );
        assert_eq!(
            first_url("see https://example.com/page)."),
            "https://example.com/page"
        );
    }
}
