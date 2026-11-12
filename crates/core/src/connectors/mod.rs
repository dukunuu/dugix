//! Connectors: an MCP client that lets the model use outside services.
//! A connector is a hosted (Streamable HTTP) or local (stdio) MCP server.

pub mod client;
pub mod config;
pub mod exec;
pub mod import;
pub mod protocol;
pub mod registry;
pub mod results;
pub mod secrets;
pub mod transport;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

pub use config::{AuthKind, ConnectorConfig, Policy};
pub use registry::{
    Access, ConnectorInfo, Env, Exposure, Registry, Status, Target, ToolRow, Tuning, display_title,
};

/// Split a command line into an argv without a shell: nothing is expanded.
pub fn split_argv(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("unterminated quote".into());
    }
    if started {
        words.push(current);
    }
    if words.is_empty() {
        return Err("the command is empty".into());
    }
    Ok(words)
}

#[cfg(test)]
mod argv_tests {
    use super::split_argv;

    #[test]
    fn command_lines_split_without_a_shell() {
        assert_eq!(
            split_argv(r#"npx -y "@scope/server name" '/Users/me/My Notes' a"b"#).unwrap_err(),
            "unterminated quote"
        );
        assert_eq!(
            split_argv(
                r#"npx -y @modelcontextprotocol/server-filesystem "/Users/me/My Notes" '' x$HOME"#
            )
            .unwrap(),
            [
                "npx",
                "-y",
                "@modelcontextprotocol/server-filesystem",
                "/Users/me/My Notes",
                "",
                "x$HOME"
            ]
        );
        assert!(split_argv("   ").is_err());
    }
}
