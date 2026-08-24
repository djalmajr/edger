use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{HeaderName, HeaderValue, CONTENT_TYPE};
use reqwest::{Method, Url};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::discovery::McpContext;

const WORKER_VERSION_HEADER: &str = "x-edger-worker-version";
const CONTROL_AUTH_HEADER: &str = "x-edger-control-authorization";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDeployedWorkersArgs {}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallWorkerArgs {
    pub zip_path: Option<String>,
    pub zip_base64: Option<String>,
    pub package_name: Option<String>,
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub force: bool,
    pub expected_revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerActionArgs {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteWorkerArgs {
    pub name: String,
    pub version: Option<String>,
    #[serde(default)]
    pub all_versions: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromoteWorkerArgs {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvokeWorkerArgs {
    pub name: String,
    pub version: Option<String>,
    #[serde(default = "default_invoke_path")]
    pub path: String,
    #[serde(default = "default_invoke_method")]
    pub method: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    pub body: Option<String>,
    pub body_base64: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservabilityEventsArgs {
    pub before: Option<u64>,
    pub limit: Option<usize>,
    pub since_ms: Option<u128>,
    pub until_ms: Option<u128>,
    pub namespace: Option<String>,
    pub worker: Option<String>,
    pub version: Option<String>,
    pub process_id: Option<String>,
    pub source: Option<String>,
    pub kind: Option<String>,
    pub level: Option<String>,
    pub outcome: Option<String>,
    pub status: Option<u16>,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub cursor: Option<u64>,
}

struct AdminClient {
    client: Client,
    base_url: Url,
    api_key: String,
}

impl AdminClient {
    fn new(ctx: &McpContext) -> Result<Self> {
        let (base_url, root_key) = ctx.control_plane()?;
        let base_url =
            Url::parse(base_url).context("configured EdgeR control-plane URL became invalid")?;
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to create EdgeR admin HTTP client")?;
        Ok(Self {
            client,
            base_url,
            api_key: root_key.to_string(),
        })
    }

    fn endpoint<'a>(&self, segments: impl IntoIterator<Item = &'a str>) -> Result<Url> {
        let mut url = self.base_url.clone();
        let mut path = url
            .path_segments_mut()
            .map_err(|_| anyhow!("EdgeR admin URL cannot accept path segments"))?;
        path.pop_if_empty();
        path.extend(segments);
        drop(path);
        Ok(url)
    }

    fn request(&self, method: Method, url: Url) -> RequestBuilder {
        self.client.request(method, url).bearer_auth(&self.api_key)
    }

    fn invoke_request(&self, method: Method, url: Url) -> RequestBuilder {
        self.client
            .request(method, url)
            .header(CONTROL_AUTH_HEADER, format!("Bearer {}", self.api_key))
    }
    fn json(&self, request: RequestBuilder) -> Result<Value> {
        parse_json_response(request.send().context("EdgeR admin request failed")?)
    }
}

pub fn install_worker(ctx: &McpContext, args: InstallWorkerArgs) -> Result<Value> {
    let bytes = match (args.zip_path.as_deref(), args.zip_base64.as_deref()) {
        (Some(path), None) => {
            let path = ctx.resolve_existing_file(args.workspace_root.as_deref(), path)?;
            fs::read(&path)
                .with_context(|| format!("failed to read deploy package {}", path.display()))?
        }
        (None, Some(encoded)) => BASE64
            .decode(encoded)
            .context("zipBase64 is not valid base64")?,
        (Some(_), Some(_)) => {
            return Err(anyhow!("provide exactly one of zipPath or zipBase64"));
        }
        (None, None) => {
            return Err(anyhow!("one of zipPath or zipBase64 is required"));
        }
    };
    // CAS obrigatório do draft: falha ANTES de qualquer HTTP, como o delete.
    if args.force && args.expected_revision.is_none() {
        return Err(anyhow!(
            "force install requires expectedRevision (the revision returned by the last install/list) — it is the compare-and-swap that stops overlapping autosaves"
        ));
    }
    let admin = AdminClient::new(ctx)?;
    let mut url = admin.endpoint(["api", "admin", "workers", "install"])?;
    if args.force {
        url.query_pairs_mut().append_pair("force", "true");
    }
    let mut request = admin
        .request(Method::POST, url)
        .header(CONTENT_TYPE, "application/zip")
        .body(bytes);
    if let Some(package_name) = args.package_name {
        request = request.header("x-edger-package-name", package_name);
    }
    if let Some(expected_revision) = args.expected_revision {
        request = request.header("x-edger-expected-revision", expected_revision);
    }
    admin.json(request)
}

pub fn list_deployed_workers(ctx: &McpContext, _args: ListDeployedWorkersArgs) -> Result<Value> {
    let admin = AdminClient::new(ctx)?;
    let url = admin.endpoint(["api", "admin", "workers"])?;
    admin.json(admin.request(Method::GET, url))
}

pub fn enable_worker(ctx: &McpContext, args: WorkerActionArgs) -> Result<Value> {
    mutate_worker(ctx, args, "enable", Method::POST)
}

pub fn disable_worker(ctx: &McpContext, args: WorkerActionArgs) -> Result<Value> {
    mutate_worker(ctx, args, "disable", Method::POST)
}

pub fn delete_worker(ctx: &McpContext, args: DeleteWorkerArgs) -> Result<Value> {
    let selected_version = match (args.version.as_deref(), args.all_versions) {
        (Some(version), false) => Some(version),
        (None, true) => None,
        (None, false) => {
            return Err(anyhow!(
                "delete requires version or explicit allVersions: true"
            ));
        }
        (Some(_), true) => {
            return Err(anyhow!(
                "version and allVersions: true are mutually exclusive"
            ));
        }
    };
    let admin = AdminClient::new(ctx)?;
    let mut url = admin.endpoint(["api", "admin", "workers", &args.name])?;
    append_optional_query(&mut url, "version", selected_version);
    admin.json(admin.request(Method::DELETE, url))
}

pub fn promote_worker(ctx: &McpContext, args: PromoteWorkerArgs) -> Result<Value> {
    let admin = AdminClient::new(ctx)?;
    let mut url = admin.endpoint(["api", "admin", "workers", &args.name, "promote"])?;
    url.query_pairs_mut().append_pair("version", &args.version);
    admin.json(admin.request(Method::POST, url))
}

pub fn invoke_worker(ctx: &McpContext, args: InvokeWorkerArgs) -> Result<Value> {
    if args.body.is_some() && args.body_base64.is_some() {
        return Err(anyhow!("body and bodyBase64 are mutually exclusive"));
    }
    if args.path.contains('?') || args.path.contains('#') {
        return Err(anyhow!(
            "path must not contain a query string or fragment; use query"
        ));
    }
    let method = Method::from_bytes(args.method.as_bytes())
        .with_context(|| format!("invalid HTTP method: {}", args.method))?;
    let admin = AdminClient::new(ctx)?;
    let mut segments = vec!["api", "admin", "workers", args.name.as_str(), "invoke"];
    segments.extend(
        args.path
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty()),
    );
    let mut url = admin.endpoint(segments)?;
    {
        let mut query = url.query_pairs_mut();
        for (name, value) in args.query {
            query.append_pair(&name, &value);
        }
    }
    let mut request = admin.invoke_request(method, url);
    for (name, value) in args.headers {
        if name.eq_ignore_ascii_case(WORKER_VERSION_HEADER)
            || name.eq_ignore_ascii_case(CONTROL_AUTH_HEADER)
        {
            return Err(anyhow!("{name} is reserved for control-plane routing"));
        }
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .with_context(|| format!("invalid invoke header name: {name}"))?;
        let value = HeaderValue::from_str(&value).context("invalid invoke header value")?;
        request = request.header(header_name, value);
    }
    if let Some(version) = args.version {
        request = request.header(WORKER_VERSION_HEADER, version);
    }
    if let Some(body) = args.body {
        request = request.body(body);
    } else if let Some(body) = args.body_base64 {
        request = request.body(
            BASE64
                .decode(body)
                .context("bodyBase64 is not valid base64")?,
        );
    }
    invocation_response(request.send().context("EdgeR invoke request failed")?)
}

pub fn list_observability_events(ctx: &McpContext, args: ObservabilityEventsArgs) -> Result<Value> {
    let admin = AdminClient::new(ctx)?;
    let mut url = admin.endpoint(["api", "admin", "observability", "events"])?;
    let mut pairs = Vec::<(String, String)>::new();
    push_number(&mut pairs, "before", args.before);
    push_number(&mut pairs, "limit", args.limit);
    push_number(&mut pairs, "sinceMs", args.since_ms);
    push_number(&mut pairs, "untilMs", args.until_ms);
    push_string(&mut pairs, "namespace", args.namespace);
    push_string(&mut pairs, "worker", args.worker);
    push_string(&mut pairs, "version", args.version);
    push_string(&mut pairs, "processId", args.process_id);
    push_string(&mut pairs, "source", args.source);
    push_string(&mut pairs, "kind", args.kind);
    push_string(&mut pairs, "level", args.level);
    push_string(&mut pairs, "outcome", args.outcome);
    push_number(&mut pairs, "status", args.status);
    push_string(&mut pairs, "requestId", args.request_id);
    push_string(&mut pairs, "traceId", args.trace_id);
    push_number(&mut pairs, "cursor", args.cursor);
    {
        let mut query = url.query_pairs_mut();
        for (name, value) in pairs {
            query.append_pair(&name, &value);
        }
    }
    admin.json(admin.request(Method::GET, url))
}

fn mutate_worker(
    ctx: &McpContext,
    args: WorkerActionArgs,
    action: &str,
    method: Method,
) -> Result<Value> {
    let admin = AdminClient::new(ctx)?;
    let mut url = admin.endpoint(["api", "admin", "workers", &args.name, action])?;
    append_optional_query(&mut url, "version", args.version.as_deref());
    admin.json(admin.request(method, url))
}

fn append_optional_query(url: &mut Url, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        url.query_pairs_mut().append_pair(name, value);
    }
}

fn push_number<T: ToString>(pairs: &mut Vec<(String, String)>, name: &str, value: Option<T>) {
    if let Some(value) = value {
        pairs.push((name.to_string(), value.to_string()));
    }
}

fn push_string(pairs: &mut Vec<(String, String)>, name: &str, value: Option<String>) {
    if let Some(value) = value {
        pairs.push((name.to_string(), value));
    }
}

fn parse_json_response(response: Response) -> Result<Value> {
    let status = response.status();
    let bytes = response
        .bytes()
        .context("failed to read EdgeR admin response")?;
    let value = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice::<Value>(&bytes).with_context(|| {
            format!("EdgeR admin returned non-JSON response with status {status}")
        })?
    };
    if status.is_success() {
        Ok(value)
    } else {
        let code = value
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("ADMIN_ERROR");
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("EdgeR admin request failed");
        Err(anyhow!("EdgeR admin returned {status}: {code}: {message}"))
    }
}

fn invocation_response(response: Response) -> Result<Value> {
    let status = response.status().as_u16();
    let mut headers = BTreeMap::<String, Vec<String>>::new();
    for (name, value) in response.headers() {
        headers
            .entry(name.as_str().to_string())
            .or_default()
            .push(value.to_str().unwrap_or("<binary>").to_string());
    }
    let bytes = response
        .bytes()
        .context("failed to read EdgeR invoke response")?;
    let body = std::str::from_utf8(&bytes).ok().map(str::to_string);
    Ok(json!({
        "status": status,
        "headers": headers,
        "body": body,
        "bodyBase64": BASE64.encode(&bytes),
    }))
}

fn default_invoke_path() -> String {
    "/".into()
}

fn default_invoke_method() -> String {
    "GET".into()
}
