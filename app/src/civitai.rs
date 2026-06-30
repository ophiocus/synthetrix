//! CivitAI REST client (blocking). Cursor pagination, throttle + 429 backoff,
//! streamed SHA256-verified downloads. API lives on civitai.com (reachable via
//! civitai.red); the token gates NSFW + creator-locked downloads.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

pub struct Client {
    http: reqwest::blocking::Client,
    base_url: String,
    token: Option<String>,
    min_interval: Duration,
    last: Instant,
    max_retries: u32,
}

pub struct ModelsPage {
    pub items: Vec<Value>,
    pub next_cursor: Option<String>,
}

impl Client {
    pub fn new(token: Option<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .user_agent("synthetrix-harvester/1.0")
            .timeout(Duration::from_secs(180))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base_url: "https://civitai.com/api/v1".into(),
            token,
            min_interval: Duration::from_millis(450), // ~130/min, backoff on 429
            last: Instant::now() - Duration::from_secs(1),
            max_retries: 5,
        }
    }

    fn throttle(&mut self) {
        let elapsed = self.last.elapsed();
        if elapsed < self.min_interval {
            std::thread::sleep(self.min_interval - elapsed);
        }
        self.last = Instant::now();
    }

    fn auth(&self, rb: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    fn get_json(&mut self, url: &str, query: &[(&str, String)]) -> Result<Value, String> {
        for attempt in 0..self.max_retries {
            self.throttle();
            let resp = self
                .auth(self.http.get(url).query(query))
                .send();
            match resp {
                Ok(r) => {
                    let code = r.status().as_u16();
                    if code == 429 || code >= 500 {
                        let wait = 2u64.pow(attempt + 1);
                        std::thread::sleep(Duration::from_secs(wait));
                        continue;
                    }
                    if !r.status().is_success() {
                        return Err(format!("HTTP {} for {}", code, url));
                    }
                    return r.json::<Value>().map_err(|e| e.to_string());
                }
                Err(e) => {
                    if attempt + 1 == self.max_retries {
                        return Err(e.to_string());
                    }
                    std::thread::sleep(Duration::from_secs(2u64.pow(attempt)));
                }
            }
        }
        Err(format!("exhausted retries for {url}"))
    }

    /// One page of /models. Cursor pagination is mandatory for deep crawls.
    pub fn models_page(
        &mut self,
        types: &str,
        base_model: &str,
        sort: &str,
        period: &str,
        nsfw: bool,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<ModelsPage, String> {
        let url = format!("{}/models", self.base_url);
        let mut q: Vec<(&str, String)> = vec![
            ("types", types.to_string()),
            ("baseModels", base_model.to_string()),
            ("sort", sort.to_string()),
            ("period", period.to_string()),
            ("nsfw", nsfw.to_string()),
            ("limit", limit.to_string()),
        ];
        if let Some(c) = cursor {
            q.push(("cursor", c.to_string()));
        }
        let v = self.get_json(&url, &q)?;
        let items = v
            .get("items")
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default();
        let next_cursor = v
            .get("metadata")
            .and_then(|m| m.get("nextCursor"))
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());
        Ok(ModelsPage { items, next_cursor })
    }

    /// Full model object by id (includes all versions + files).
    pub fn model_by_id(&mut self, id: i64) -> Result<Value, String> {
        let url = format!("{}/models/{}", self.base_url, id);
        self.get_json(&url, &[])
    }

    /// Identify a local file by hash. Returns the model-version object, or None
    /// if CivitAI has no match (404). Accepts SHA256/AutoV2/Blake3/etc.
    pub fn model_version_by_hash(&mut self, hash: &str) -> Result<Option<Value>, String> {
        let url = format!("{}/model-versions/by-hash/{}", self.base_url, hash);
        for attempt in 0..self.max_retries {
            self.throttle();
            match self.auth(self.http.get(&url)).send() {
                Ok(r) => {
                    let code = r.status().as_u16();
                    if code == 404 {
                        return Ok(None);
                    }
                    if code == 429 || code >= 500 {
                        std::thread::sleep(Duration::from_secs(2u64.pow(attempt + 1)));
                        continue;
                    }
                    if !r.status().is_success() {
                        return Err(format!("HTTP {code} (by-hash)"));
                    }
                    return r.json::<Value>().map(Some).map_err(|e| e.to_string());
                }
                Err(e) => {
                    if attempt + 1 == self.max_retries {
                        return Err(e.to_string());
                    }
                    std::thread::sleep(Duration::from_secs(2u64.pow(attempt)));
                }
            }
        }
        Err("exhausted retries (by-hash)".into())
    }

    /// Fetch raw bytes (images/covers). Hits image.civitai.com CDN — not the
    /// rate-limited API — so no throttle.
    pub fn get_bytes(&mut self, url: &str) -> Result<(String, Vec<u8>), String> {
        let r = self
            .auth(self.http.get(url))
            .send()
            .map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(format!("HTTP {} for image", r.status().as_u16()));
        }
        let ct = r
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .to_string();
        let bytes = r.bytes().map_err(|e| e.to_string())?.to_vec();
        Ok((ct, bytes))
    }

    /// Stream a model file to `dest`, hashing as we go. `progress(done,total)`.
    /// Returns the lowercase SHA256 hex.
    pub fn download_file(
        &mut self,
        url: &str,
        dest: &Path,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<String, String> {
        // Token must ride in the query string: CivitAI 302-redirects downloads to
        // its S3/CDN host, and reqwest strips the Authorization header on cross-host
        // redirects — so bearer_auth alone loses the credential for gated/NSFW files.
        let url = match &self.token {
            Some(t) => {
                let sep = if url.contains('?') { '&' } else { '?' };
                format!("{url}{sep}token={t}")
            }
            None => url.to_string(),
        };
        let tmp = dest.with_extension("part");
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // Retry the whole download on 429 / 5xx / transient stream errors, with
        // exponential backoff that honors a Retry-After header when present.
        for attempt in 0..self.max_retries {
            self.throttle();
            let mut r = match self.auth(self.http.get(url.as_str())).send() {
                Ok(r) => r,
                Err(e) => {
                    if attempt + 1 == self.max_retries {
                        return Err(e.to_string());
                    }
                    std::thread::sleep(Duration::from_secs(2u64.pow(attempt)));
                    continue;
                }
            };
            let code = r.status().as_u16();
            if code == 429 || code >= 500 {
                let wait = retry_after(&r).unwrap_or_else(|| 2u64.pow(attempt + 1)).min(60);
                std::thread::sleep(Duration::from_secs(wait));
                continue;
            }
            if !r.status().is_success() {
                return Err(format!("HTTP {code} downloading"));
            }
            // A stripped-auth response is often a 200 HTML login page — never a model.
            if let Some(ct) = r
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
            {
                if ct.starts_with("text/html") {
                    return Err("got HTML (auth/redirect failure), not a model file".into());
                }
            }

            let total = r.content_length().unwrap_or(0);
            let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
            let mut hasher = Sha256::new();
            let mut buf = [0u8; 1 << 20];
            let mut done = 0u64;
            let mut stream_err: Option<String> = None;
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        hasher.update(&buf[..n]);
                        if let Err(e) = f.write_all(&buf[..n]) {
                            stream_err = Some(e.to_string());
                            break;
                        }
                        done += n as u64;
                        progress(done, total);
                    }
                    Err(e) => {
                        stream_err = Some(e.to_string());
                        break;
                    }
                }
            }
            if let Some(e) = stream_err {
                drop(f);
                let _ = std::fs::remove_file(&tmp);
                if attempt + 1 == self.max_retries {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_secs(2u64.pow(attempt)));
                continue;
            }
            f.flush().ok();
            drop(f);
            std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
            return Ok(format!("{:x}", hasher.finalize()));
        }
        Err("rate-limited (429) — exhausted retries; try again shortly".into())
    }
}

/// Parse a Retry-After header (seconds form) into a duration in seconds.
fn retry_after(r: &reqwest::blocking::Response) -> Option<u64> {
    r.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

#[cfg(test)]
mod net_probe {
    use super::*;
    use std::time::Instant;

    fn token() -> Option<String> {
        std::env::var("CIVITAI_TOKEN").ok().or_else(|| {
            std::fs::read_to_string("../.env").ok().and_then(|s| {
                s.lines().find_map(|l| {
                    l.trim().strip_prefix("CIVITAI_TOKEN=").map(|x| x.trim().to_string())
                })
            })
        })
    }

    #[test]
    #[ignore]
    fn probe() {
        let tok = token();
        println!("token present: {}", tok.is_some());
        let mut c = Client::new(tok);
        let t = Instant::now();
        let r = c.models_page("Checkpoint", "Flux.1 D", "Most Downloaded", "AllTime", true, 5, None);
        println!("models_page elapsed: {:?}", t.elapsed());
        match r {
            Ok(p) => println!("OK items={} next_cursor={}", p.items.len(), p.next_cursor.is_some()),
            Err(e) => println!("ERR {e}"),
        }
    }
}
