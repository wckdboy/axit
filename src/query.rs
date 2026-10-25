use crate::index::{Entry, Index};
use anyhow::{Context, Result, bail, ensure};
use regex::bytes::{Regex, RegexBuilder};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, sync::atomic::AtomicU64};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Name,
    Size,
    Modified,
}

pub struct Query {
    input_hash: [u8; 32],
    terms: Vec<(Regex, bool)>,
    paths: Vec<Regex>,
    extensions: Vec<Vec<u8>>,
    sizes: Vec<(char, u64)>,
    older: Option<i64>,
    folder: Option<bool>,
    scope: Option<u32>,
}

pub use crate::page::Page as SearchResult;

pub fn words(input: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    for c in input.chars() {
        if escaped {
            token.push(c);
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                token.push(c);
            }
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            continue;
        }
        if c.is_whitespace() {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(c);
        }
    }
    ensure!(quote.is_none(), "Close the quoted search term");
    if escaped {
        token.push('\\');
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Ok(tokens)
}

fn pattern(input: &str, raw: bool) -> Result<Regex> {
    let p = if raw {
        input.to_string()
    } else if input.contains(['*', '?']) {
        format!(
            "^{}$",
            regex::escape(input)
                .replace("\\*", ".*")
                .replace("\\?", ".")
        )
    } else {
        regex::escape(input)
    };
    Ok(RegexBuilder::new(&p)
        .case_insensitive(true)
        .unicode(false)
        .size_limit(2 * 1024 * 1024)
        .dfa_size_limit(2 * 1024 * 1024)
        .build()?)
}

pub fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim().to_ascii_lowercase();
    let split = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let n: f64 = s[..split]
        .parse()
        .context("Use a size such as 500mb or 2gib")?;
    let factor = match s[split..].trim() {
        "" | "b" => 1f64,
        "k" | "kb" => 1e3,
        "m" | "mb" => 1e6,
        "g" | "gb" => 1e9,
        "t" | "tb" => 1e12,
        "kib" => 1024.,
        "mib" => 1024f64.powi(2),
        "gib" => 1024f64.powi(3),
        "tib" => 1024f64.powi(4),
        _ => bail!("Unknown size unit"),
    };
    ensure!(
        n.is_finite() && n >= 0. && n * factor <= u64::MAX as f64,
        "Size is out of range"
    );
    Ok((n * factor) as u64)
}

impl Query {
    pub fn parse(input: &str) -> Result<Self> {
        ensure!(input.len() <= 4096, "Search is too long");
        let mut q = Self {
            input_hash: Sha256::digest(input.as_bytes()).into(),
            terms: Vec::new(),
            paths: Vec::new(),
            extensions: Vec::new(),
            sizes: Vec::new(),
            older: None,
            folder: None,
            scope: None,
        };
        for token in words(input)? {
            if let Some(s) = token.strip_prefix("ext:") {
                q.extensions = s
                    .split([';', ','])
                    .map(|s| s.trim_start_matches('.').as_bytes().to_ascii_lowercase())
                    .collect();
            } else if let Some(s) = token.strip_prefix("size:") {
                let (op, n) = if let Some(s) = s.strip_prefix('>') {
                    ('>', s)
                } else if let Some(s) = s.strip_prefix('<') {
                    ('<', s)
                } else if let Some(s) = s.strip_prefix('=') {
                    ('=', s)
                } else {
                    ('>', s)
                };
                q.sizes.push((op, parse_size(n)?));
            } else if let Some(s) = token.strip_prefix("older:") {
                let days: i64 = s.trim_end_matches('d').parse().context("Use older:30d")?;
                ensure!((0..=365000).contains(&days), "Age is out of range");
                q.older = Some(days);
            } else if let Some(s) = token.strip_prefix("path:") {
                q.paths.push(pattern(s, false)?);
            } else if token == "type:dir" || token == "folder:" {
                q.folder = Some(true);
            } else if token == "type:file" || token == "file:" {
                q.folder = Some(false);
            } else if let Some(s) = token.strip_prefix("re:") {
                q.terms.push((pattern(s, true)?, false));
            } else if let Some(s) = token.strip_prefix('!') {
                q.terms.push((pattern(s, false)?, true));
            } else {
                q.terms.push((pattern(&token, false)?, false));
            }
        }
        Ok(q)
    }

    pub fn set_scope(&mut self, id: u32) {
        self.scope = Some(id);
    }

    fn matches(&self, index: &Index, e: Entry, as_of: i64) -> bool {
        if self
            .scope
            .is_some_and(|scope| !index.is_descendant(e.id, scope))
        {
            return false;
        }
        if self.folder.is_some_and(|dir| dir != e.is_dir()) {
            return false;
        }
        if self
            .older
            .is_some_and(|days| e.modified > as_of.saturating_sub(days * 86400))
        {
            return false;
        }
        for &(op, n) in &self.sizes {
            if match op {
                '>' => e.allocated <= n,
                '<' => e.allocated >= n,
                _ => e.allocated != n,
            } {
                return false;
            }
        }
        let name = index.name(e);
        if !self.extensions.is_empty() {
            let extension = name.rsplit(|c| *c == b'.').next().unwrap_or_default();
            if e.is_dir()
                || !self
                    .extensions
                    .iter()
                    .any(|x| extension.eq_ignore_ascii_case(x))
            {
                return false;
            }
        }
        if self
            .terms
            .iter()
            .any(|(r, negate)| r.is_match(name) == *negate)
        {
            return false;
        }
        if !self.paths.is_empty() {
            use std::os::unix::ffi::OsStrExt;
            let Ok(path) = index.path(e.id) else {
                return false;
            };
            if self
                .paths
                .iter()
                .any(|r| !r.is_match(path.as_os_str().as_bytes()))
            {
                return false;
            }
        }
        true
    }
}

impl Query {
    fn page_spec(&self, sort: Sort, hidden: &HashSet<u32>) -> crate::page::Spec {
        let mut key = b"search".to_vec();
        key.extend_from_slice(&self.input_hash);
        key.extend_from_slice(&self.scope.unwrap_or(u32::MAX).to_le_bytes());
        crate::page::Spec {
            sort,
            binding: crate::page::binding(&key, sort, hidden),
            summarize: false,
        }
    }
    pub fn cursor_at(&self, index: &Index, sort: Sort, hidden: &HashSet<u32>, id: u32) -> String {
        crate::page::at(index, self.page_spec(sort, hidden), id)
    }
}
pub fn search(
    index: &Index,
    query: &Query,
    sort: Sort,
    limit: usize,
    hidden: &HashSet<u32>,
    generation: Option<(&AtomicU64, u64)>,
) -> SearchResult {
    crate::page::first(
        index,
        query.page_spec(sort, hidden),
        limit,
        0..index.count,
        |e, as_of| !index.is_hidden(e.id, hidden) && query.matches(index, e, as_of),
        generation,
    )
}
pub fn search_page(
    index: &Index,
    query: &Query,
    sort: Sort,
    request: crate::page::Request<'_>,
    hidden: &HashSet<u32>,
    generation: Option<(&AtomicU64, u64)>,
) -> Result<SearchResult> {
    crate::page::select(
        index,
        query.page_spec(sort, hidden),
        request,
        0..index.count,
        |e, as_of| !index.is_hidden(e.id, hidden) && query.matches(index, e, as_of),
        generation,
    )
}
