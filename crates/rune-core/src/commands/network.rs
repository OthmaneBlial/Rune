use super::super::{NetworkMethod, NetworkRequest};
use crate::{usage, CommandContext, CommandOutput};

const CURL_FAILURE_STATUS: i32 = 22;
const DEFAULT_DOH_SERVER: &str = "https://cloudflare-dns.com/dns-query";
const DEFAULT_RDAP_SERVER: &str = "https://rdap.org/domain/";
const MAX_DNS_NAME_BYTES: usize = 253;
const MAX_DNS_LABEL_BYTES: usize = 63;
const MAX_DNS_ANSWERS: usize = 128;
const MAX_DNS_DATA_BYTES: usize = 16 * 1024;
const MAX_WHOIS_OUTPUT_BYTES: usize = 256 * 1024;
const DNS_RECORD_TYPES: &[&str] = &[
    "A", "AAAA", "CAA", "CNAME", "MX", "NS", "PTR", "SOA", "SRV", "TXT",
];

/// Resolve one name through a host-provided DNS-over-HTTPS endpoint.
///
/// Rune deliberately exposes a bounded, non-interactive resolver instead of
/// opening DNS sockets in the portable core. The host network provider still
/// owns the actual HTTPS transport.
pub(super) fn nslookup(context: &mut CommandContext<'_>) -> CommandOutput {
    dns_lookup(context, "nslookup")
}

/// Resolve one name through the same bounded `DoH` surface under the common
/// `host` command name used by a-Shell and other Unix environments.
pub(super) fn host(context: &mut CommandContext<'_>) -> CommandOutput {
    dns_lookup(context, "host")
}

fn dns_lookup(context: &mut CommandContext<'_>, command: &str) -> CommandOutput {
    let options = match parse_nslookup_args(context.args, command) {
        Ok(options) => options,
        Err(message) => return usage(command, &message),
    };
    let url = build_doh_url(&options.server, &options.host, &options.record_type);
    let request = NetworkRequest {
        method: NetworkMethod::Get,
        url,
        headers: vec![("Accept".to_string(), "application/dns-json".to_string())],
        body: Vec::new(),
    };
    if let Err(error) = request.validate() {
        return CommandOutput::failure(2, format!("{command}: {error}\n"));
    }
    let response = match context.network.request(&request) {
        Ok(response) => response,
        Err(error) => return CommandOutput::failure(1, format!("{command}: {error}\n")),
    };
    if let Err(error) = response.validate() {
        return CommandOutput::failure(1, format!("{command}: {error}\n"));
    }
    if !(200..300).contains(&response.status_code) {
        return CommandOutput::failure(
            1,
            format!(
                "{command}: resolver returned HTTP status {}\n",
                response.status_code
            ),
        );
    }

    let Ok(document) = serde_json::from_slice::<serde_json::Value>(&response.body) else {
        return CommandOutput::failure(1, format!("{command}: invalid DNS JSON response\n"));
    };
    let Some(status) = document.get("Status").and_then(serde_json::Value::as_u64) else {
        return CommandOutput::failure(1, format!("{command}: DNS response has no status\n"));
    };
    if status != 0 {
        return CommandOutput::failure(
            1,
            format!(
                "{command}: resolver returned DNS status {status} for {}\n",
                options.host
            ),
        );
    }

    let mut stdout = String::new();
    if let Some(answers) = document.get("Answer").and_then(serde_json::Value::as_array) {
        for answer in answers.iter().take(MAX_DNS_ANSWERS) {
            let Some(data) = answer.get("data").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if data.is_empty() || data.len() > MAX_DNS_DATA_BYTES {
                continue;
            }
            if data.chars().any(char::is_control) {
                return CommandOutput::failure(
                    1,
                    format!("{command}: DNS response contains control data\n"),
                );
            }
            stdout.push_str(data);
            stdout.push('\n');
        }
    }
    if stdout.is_empty() {
        return CommandOutput::failure(
            1,
            format!(
                "{command}: no {} records found for {}\n",
                options.record_type, options.host
            ),
        );
    }
    CommandOutput::success(stdout)
}

/// Fetch a domain's RDAP record through a host-provided HTTPS endpoint.
///
/// Traditional WHOIS uses an ambient TCP connection to port 43. Rune keeps
/// that transport outside the portable core and exposes the interoperable
/// HTTPS RDAP form instead, with a bounded text response.
pub(super) fn whois(context: &mut CommandContext<'_>) -> CommandOutput {
    let options = match parse_whois_args(context.args) {
        Ok(options) => options,
        Err(message) => return usage("whois", &message),
    };
    let request = NetworkRequest {
        method: NetworkMethod::Get,
        url: build_rdap_url(&options.server, &options.target),
        headers: vec![(
            "Accept".to_string(),
            "application/rdap+json, application/json, text/plain".to_string(),
        )],
        body: Vec::new(),
    };
    if let Err(error) = request.validate() {
        return CommandOutput::failure(2, format!("whois: {error}\n"));
    }
    let response = match context.network.request(&request) {
        Ok(response) => response,
        Err(error) => return CommandOutput::failure(1, format!("whois: {error}\n")),
    };
    if let Err(error) = response.validate() {
        return CommandOutput::failure(1, format!("whois: {error}\n"));
    }
    if !(200..300).contains(&response.status_code) {
        return CommandOutput::failure(
            1,
            format!(
                "whois: RDAP server returned HTTP status {}\n",
                response.status_code
            ),
        );
    }
    if response.body.len() > MAX_WHOIS_OUTPUT_BYTES {
        return CommandOutput::failure(
            1,
            format!("whois: response exceeds {MAX_WHOIS_OUTPUT_BYTES} bytes\n"),
        );
    }
    let Ok(stdout) = String::from_utf8(response.body) else {
        return CommandOutput::failure(1, "whois: response is not valid UTF-8\n");
    };
    if stdout.is_empty() {
        return CommandOutput::failure(1, "whois: RDAP server returned an empty response\n");
    }
    if stdout
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return CommandOutput::failure(1, "whois: response contains terminal control data\n");
    }
    CommandOutput::success(stdout)
}

#[derive(Debug, PartialEq, Eq)]
struct NslookupOptions {
    server: String,
    record_type: String,
    host: String,
}

#[derive(Debug, PartialEq, Eq)]
struct WhoisOptions {
    server: String,
    target: String,
}

fn parse_nslookup_args(args: &[String], command: &str) -> Result<NslookupOptions, String> {
    let mut server = DEFAULT_DOH_SERVER.to_string();
    let mut record_type = "A".to_string();
    let mut host = None;
    let mut parse_options = true;
    let mut index = 0;

    while index < args.len() {
        let argument = &args[index];
        if parse_options && argument == "--" {
            parse_options = false;
            index += 1;
            continue;
        }
        if parse_options {
            let (option, inline_value) = argument
                .split_once('=')
                .filter(|(option, _)| {
                    matches!(
                        *option,
                        "-s" | "--server" | "-type" | "--type" | "-q" | "--querytype"
                    )
                })
                .map_or((argument.as_str(), None), |(option, value)| {
                    (option, Some(value))
                });
            match option {
                "-s" | "--server" => {
                    server = option_value(args, &mut index, inline_value, option)?;
                }
                "-type" | "--type" | "-q" | "--querytype" => {
                    record_type =
                        option_value(args, &mut index, inline_value, option)?.to_ascii_uppercase();
                    if !DNS_RECORD_TYPES.contains(&record_type.as_str()) {
                        return Err(format!(
                            "unsupported record type {}; choose one of {}",
                            record_type,
                            DNS_RECORD_TYPES.join(", ")
                        ));
                    }
                }
                value if value.starts_with('-') => {
                    return Err(format!(
                        "usage: {command} [--server SERVER] [-type=TYPE] HOST"
                    ));
                }
                value => set_nslookup_host(&mut host, value.to_string())?,
            }
        } else {
            set_nslookup_host(&mut host, argument.clone())?;
        }
        index += 1;
    }

    let host =
        host.ok_or_else(|| format!("usage: {command} [--server SERVER] [-type=TYPE] HOST"))?;
    validate_dns_name(&host)?;
    if !server
        .get(.."https://".len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    {
        return Err("--server must use an https:// DNS-over-HTTPS endpoint".to_string());
    }
    if server.contains('#') {
        return Err("--server must not contain a URL fragment".to_string());
    }
    Ok(NslookupOptions {
        server,
        record_type,
        host,
    })
}

fn parse_whois_args(args: &[String]) -> Result<WhoisOptions, String> {
    let mut server = DEFAULT_RDAP_SERVER.to_string();
    let mut target = None;
    let mut parse_options = true;
    let mut index = 0;

    while index < args.len() {
        let argument = &args[index];
        if parse_options && argument == "--" {
            parse_options = false;
            index += 1;
            continue;
        }
        if parse_options {
            let (option, inline_value) = argument
                .split_once('=')
                .filter(|(option, _)| matches!(*option, "-s" | "--server"))
                .map_or((argument.as_str(), None), |(option, value)| {
                    (option, Some(value))
                });
            match option {
                "-s" | "--server" => {
                    server = option_value(args, &mut index, inline_value, option)?;
                }
                value if value.starts_with('-') => {
                    return Err("usage: whois [--server SERVER] DOMAIN".to_string());
                }
                value => set_whois_target(&mut target, value.to_string())?,
            }
        } else {
            set_whois_target(&mut target, argument.clone())?;
        }
        index += 1;
    }

    let target = target.ok_or_else(|| "usage: whois [--server SERVER] DOMAIN".to_string())?;
    validate_dns_name(&target)?;
    if !server
        .get(.."https://".len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    {
        return Err("--server must use an https:// RDAP endpoint".to_string());
    }
    if server.contains('?') || server.contains('#') {
        return Err("--server must not contain a query or URL fragment".to_string());
    }
    Ok(WhoisOptions { server, target })
}

fn option_value(
    args: &[String],
    index: &mut usize,
    inline_value: Option<&str>,
    option: &str,
) -> Result<String, String> {
    if let Some(value) = inline_value {
        return (!value.is_empty())
            .then(|| value.to_string())
            .ok_or_else(|| format!("{option} requires a value"));
    }
    *index += 1;
    args.get(*index)
        .filter(|value| !value.is_empty() && !value.starts_with('-'))
        .cloned()
        .ok_or_else(|| format!("{option} requires a value"))
}

fn set_nslookup_host(host: &mut Option<String>, value: String) -> Result<(), String> {
    if host.is_some() {
        return Err("only one host name is supported".to_string());
    }
    *host = Some(value);
    Ok(())
}

fn set_whois_target(target: &mut Option<String>, value: String) -> Result<(), String> {
    if target.is_some() {
        return Err("only one domain is supported".to_string());
    }
    *target = Some(value);
    Ok(())
}

fn validate_dns_name(host: &str) -> Result<(), String> {
    let name = host.strip_suffix('.').unwrap_or(host);
    if name.is_empty() || name.len() > MAX_DNS_NAME_BYTES {
        return Err(format!(
            "host name must contain 1-{MAX_DNS_NAME_BYTES} bytes"
        ));
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-' || byte == b'_')
    {
        return Err("host name must contain only ASCII DNS label characters".to_string());
    }
    for label in name.split('.') {
        if label.is_empty() || label.len() > MAX_DNS_LABEL_BYTES {
            return Err(format!(
                "each DNS label must contain 1-{MAX_DNS_LABEL_BYTES} bytes"
            ));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("DNS labels cannot start or end with '-'".to_string());
        }
    }
    Ok(())
}

fn build_doh_url(server: &str, host: &str, record_type: &str) -> String {
    let separator = if server.contains('?') { '&' } else { '?' };
    format!(
        "{server}{separator}name={}&type={record_type}",
        percent_encode(host)
    )
}

fn build_rdap_url(server: &str, target: &str) -> String {
    let separator = if server.ends_with('/') { "" } else { "/" };
    format!("{server}{separator}{}", percent_encode(target))
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

#[allow(clippy::too_many_lines)]
pub(super) fn curl(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut method = None;
    let mut body = None;
    let mut headers = Vec::new();
    let mut output_path = None;
    let mut fail_on_http_error = false;
    let mut url: Option<String> = None;
    let mut parse_options = true;
    let mut index = 0;

    while index < context.args.len() {
        let argument = &context.args[index];
        if parse_options && argument == "--" {
            parse_options = false;
            index += 1;
            continue;
        }
        if parse_options {
            match argument.as_str() {
                "-I" | "--head" => method = Some(NetworkMethod::Head),
                "-f" | "--fail" => fail_on_http_error = true,
                "-s" | "--silent" => {}
                "-X" | "--request" => {
                    index += 1;
                    let Some(value) = context.args.get(index) else {
                        return usage("curl", "-X/--request requires METHOD");
                    };
                    method = match NetworkMethod::parse(value) {
                        Ok(method) => Some(method),
                        Err(error) => return CommandOutput::failure(2, format!("curl: {error}\n")),
                    };
                }
                "-d" | "--data" | "--data-raw" | "--data-binary" => {
                    index += 1;
                    let Some(value) = context.args.get(index) else {
                        return usage("curl", "data option requires DATA");
                    };
                    if body.is_some() {
                        return usage("curl", "only one data option is supported");
                    }
                    body = Some(value.as_bytes().to_vec());
                }
                "-H" | "--header" => {
                    index += 1;
                    let Some(value) = context.args.get(index) else {
                        return usage("curl", "header option requires NAME: VALUE");
                    };
                    let Some((name, value)) = value.split_once(':') else {
                        return usage("curl", "header must use NAME: VALUE");
                    };
                    headers.push((name.trim().to_string(), value.trim().to_string()));
                }
                "-o" | "--output" => {
                    index += 1;
                    let Some(value) = context.args.get(index) else {
                        return usage("curl", "output option requires FILE");
                    };
                    if output_path.is_some() {
                        return usage("curl", "only one output file is supported");
                    }
                    output_path = Some(value.clone());
                }
                value if value.starts_with('-') => {
                    return usage(
                        "curl",
                        "usage: curl [-I|-f|-s] [-X METHOD] [-H NAME:VALUE] [-d DATA] [-o FILE] URL",
                    );
                }
                value => {
                    if url.is_some() {
                        return usage("curl", "only one URL is supported");
                    }
                    url = Some(value.to_string());
                }
            }
        } else {
            if url.is_some() {
                return usage("curl", "only one URL is supported");
            }
            url = Some(argument.clone());
        }
        index += 1;
    }

    let Some(url) = url else {
        return usage(
            "curl",
            "usage: curl [-I|-f|-s] [-X METHOD] [-H NAME:VALUE] [-d DATA] [-o FILE] URL",
        );
    };
    let method = method.unwrap_or(if body.is_some() {
        NetworkMethod::Post
    } else {
        NetworkMethod::Get
    });
    if method == NetworkMethod::Head && body.is_some() {
        return usage("curl", "HEAD requests cannot include DATA");
    }
    let request = NetworkRequest {
        method,
        url,
        headers,
        body: body.unwrap_or_default(),
    };
    if let Err(error) = request.validate() {
        return CommandOutput::failure(2, format!("curl: {error}\n"));
    }
    let response = match context.network.request(&request) {
        Ok(response) => response,
        Err(error) => return CommandOutput::failure(1, format!("curl: {error}\n")),
    };
    if let Err(error) = response.validate() {
        return CommandOutput::failure(1, format!("curl: {error}\n"));
    }
    if fail_on_http_error && response.status_code >= 400 {
        return CommandOutput::failure(
            CURL_FAILURE_STATUS,
            format!("curl: HTTP status {}\n", response.status_code),
        );
    }

    if let Some(path) = output_path {
        if path != "-" {
            return context.fs.write(&path, &response.body, false).map_or_else(
                |error| CommandOutput::failure(1, format!("curl: {error}\n")),
                |()| CommandOutput::success(""),
            );
        }
    }
    match String::from_utf8(response.body) {
        Ok(stdout) => CommandOutput::success(stdout),
        Err(_) => CommandOutput::failure(1, "curl: response is binary; use -o FILE to save it\n"),
    }
}
