//! Environment-variable injection for the `serve` subcommand.
//!
//! Workspace AGENT.md requires servers to accept their settings from both API
//! arguments and environment variables. Precedence is explicit argument over
//! environment over the flag's default, so an operator can bake a backend URL
//! into a unit file without changing the command line.

/// Environment variable naming the llama-server base URL for `serve`.
pub const SERVER_ENV: &str = "EOMER_SRT_OCR_SERVER";

/// Environment variable naming the bind address for `serve`.
pub const BIND_ENV: &str = "EOMER_SRT_OCR_BIND";

/// Environment variable naming the working width for `serve`.
pub const WIDTH_ENV: &str = "EOMER_SRT_OCR_WIDTH";

/// Apply the environment fallback to an explicitly-passed value.
///
/// An empty or whitespace-only variable is treated as unset: an exported but
/// empty variable is a common shell accident and must not override a default.
pub fn env_or(explicit: Option<String>, var: &str) -> Option<String> {
    explicit.or_else(|| {
        std::env::var(var)
            .ok()
            .filter(|v| !v.trim().is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_value_wins_over_environment() {
        unsafe { std::env::set_var(SERVER_ENV, "http://env.example:1") };
        let picked = env_or(Some("http://flag.example:2".into()), SERVER_ENV);
        assert_eq!(picked.as_deref(), Some("http://flag.example:2"));
        unsafe { std::env::remove_var(SERVER_ENV) };
    }

    #[test]
    fn environment_fills_in_when_no_argument_was_given() {
        unsafe { std::env::set_var(BIND_ENV, "127.0.0.1:9999") };
        let picked = env_or(None, BIND_ENV);
        assert_eq!(picked.as_deref(), Some("127.0.0.1:9999"));
        unsafe { std::env::remove_var(BIND_ENV) };
        assert_eq!(env_or(None, BIND_ENV), None);
    }

    #[test]
    fn blank_variable_is_treated_as_unset() {
        unsafe { std::env::set_var(WIDTH_ENV, "   ") };
        assert_eq!(env_or(None, WIDTH_ENV), None, "blank must not override");
        unsafe { std::env::remove_var(WIDTH_ENV) };
    }
}