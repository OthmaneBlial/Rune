use super::super::{NetworkMethod, NetworkRequest};
use crate::{usage, CommandContext, CommandOutput};

const CURL_FAILURE_STATUS: i32 = 22;

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
