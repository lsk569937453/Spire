use clap::Parser;

use super::app_config::AppConfig;
use clap::Args;
use clap::Subcommand;
use clap::ValueEnum;
use std::path::PathBuf;
use std::sync::Arc;

use std::sync::RwLock;
#[derive(ValueEnum, Debug, Clone)]
pub enum InputFormat {
    Openapi,
    Swagger,
}
#[derive(Parser, Debug, Clone)]
#[command(
    name = "Spire",
    version = crate_version!(),
    about = concat!("The Spire API Gateway v", crate_version!()),
    long_about = None,
    after_help = "\
GETTING STARTED:
    spire -f config.yaml
        Start the gateway with a configuration file

    spire examples --list
        List all available configuration examples

    spire examples <name>
        Display a specific configuration example

    spire validate -c config.yaml
        Validate a configuration file before starting

    spire config expand -c config.yaml
        Print the fully expanded configuration (like 'nginx -T')

    spire convert openapi.yaml -o config.yaml
        Convert OpenAPI/Swagger spec to gateway config

    spire reload --pid 1234
        Reload the running process (sends SIGHUP, like 'nginx -s reload')

COMMANDS:
    config (cfg)     Configuration utilities (expand)
    convert (conv)   Convert OpenAPI/Swagger files to gateway configuration
    examples (ex)    List and display configuration examples
    reload (rel)     Signal a running process to reload its configuration
    validate (val)   Validate a configuration file

CONFIGURATION:
    Configuration files use YAML format. See 'spire examples' for available examples.
    Default config file: config.yaml
    Use 'spire validate' to check your config before starting
    There is no HTTP admin API: apply changes with 'spire reload --pid <pid>' (SIGHUP).

BUILD WITH PROFILING:
    cargo build -rF pprof
        Build release binary with pprof flamegraph support

RESOURCES:
    Documentation: https://github.com/lsk569937453/spire
    Examples:      spire examples --list
    Issue Tracker:  https://github.com/lsk569937453/spire/issues"
)]
pub struct Cli {
    #[arg(short = 'f', long, default_value = "config.yaml")]
    pub config_path: String,
    #[command(subcommand)]
    pub command: Option<Commands>,
}
#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    #[command(
        visible_alias = "conv",
        about = "Converts an OpenAPI/Swagger file into a gateway configuration",
        long_about = "Convert an OpenAPI 3.0 or Swagger 2.0 specification file into a Spire gateway configuration file.",
        after_help = "\
EXAMPLES:
    spire convert openapi.yaml
        Convert OpenAPI spec and output to stdout

    spire convert openapi.yaml -o config.yaml
        Convert OpenAPI spec and save to config.yaml

    spire conv swagger.json --format Swagger -o gateway.yaml
        Convert Swagger spec using the 'conv' alias

    spire convert petstore.yaml -o config/petstore.yaml
        Convert and save to a specific directory

SUPPORTED FORMATS:
    Openapi (default)   OpenAPI 3.0.x specification files
    Swagger             Swagger 2.0 specification files

CONVERSION DETAILS:
    - Extracts all paths and HTTP methods from the spec
    - Creates route configurations with path and method matchers
    - Path parameters (e.g., /users/{id}) are converted to regex matchers
    - Uses server URLs from the spec as upstream endpoints
    - If no servers are specified, defaults to http://127.0.0.1:8080
    - Generates random router configuration for each route

OUTPUT:
    The converted configuration is printed to stdout in YAML format.
    Use the -o option to save to a file instead.

For more information: https://github.com/lsk569937453/spire"
    )]
    Convert(ConvertArgs),
    #[command(
        visible_alias = "ex",
        about = "List and display configuration examples",
        long_about = "List all available configuration examples or display a specific example by name. Use --list to see all available examples.",
        after_help = "\
EXAMPLES:
    spire examples --list
        List all available configuration examples with descriptions

    spire examples app_config_simple
        Display the minimal configuration example

    spire examples forward_ip
        Display IP forwarding examples (partial name match)

    spire examples weight
        Find and display weight-based routing example

AVAILABLE CATEGORIES:
    Basic Configuration
        app_config_simple, app_config_https

    Routing Strategies
        http_weight_route, http_random_route, http_poll_route, http_header_based_route

    Middleware Features
        forward_ip_examples, request_headers, http_cors, jwt_auth, middle_wares,
        compression, ip_ban

    Security
        trusted_proxies, jwt_auth, ip_ban

    Rate Limiting
        ratelimit_token_bucket, ratelimit_fixed_window

    Advanced Features
        health_check, circuit_breaker, matchers, reverse_proxy, tcp_proxy,
        http_to_grpc, static_file, openapi_convert

For more information, visit: https://github.com/lsk569937453/spire"
    )]
    Examples(ExamplesArgs),
    #[command(
        visible_alias = "val",
        about = "Validate a configuration file",
        long_about = "Validate a Spire configuration file for syntax errors and structural issues.",
        after_help = "\
EXAMPLES:
    spire validate
        Validate the default config.yaml file

    spire validate -c custom_config.yaml
        Validate a specific configuration file

    spire validate --config config/examples/app_config_simple.yaml
        Validate with long option name

    spire val -c config.yaml --verbose
        Validate with detailed output (using alias)

CHECKS PERFORMED:
    - YAML syntax validation
    - Configuration structure validation
    - Type checking for all fields

EXIT CODES:
    0   Configuration is valid
    1   Configuration is invalid (YAML syntax or deserialization error)
    2   Error reading file (file not found, permission denied, etc.)

COMMON ERRORS:
    - Invalid YAML syntax (e.g., indentation issues, unmatched brackets)
    - Unknown configuration fields
    - Invalid field values (e.g., negative port numbers)
    - Missing required fields"
    )]
    Validate(ValidateArgs),
    #[command(
        visible_alias = "rel",
        about = "Reload the running process (sends SIGHUP)",
        long_about = "Send SIGHUP to a running Spire process, which makes it re-read its configuration file. \
                      Equivalent to 'nginx -s reload'.",
        after_help = "\
EXAMPLES:
    spire reload --pid 1234
        Reload the process with PID 1234 from its own configuration file

    systemd:
        ExecReload=/bin/kill -HUP $MAINPID

    docker:
        docker kill -s HUP <container>

WHAT HAPPENS ON RELOAD:
    - The configuration file is read again (the same one the process was started with)
    - A file that fails to parse is logged and ignored: the previous configuration keeps running
    - Listeners may be added and removed
    - Changing the protocol or the domains of an existing port requires a restart
    - The observability listener address requires a restart

PLATFORMS:
    SIGHUP only exists on Unix. On Windows, restart the process instead."
    )]
    Reload(ReloadArgs),
    #[command(
        visible_alias = "cfg",
        about = "Configuration utilities",
        long_about = "Inspect the configuration without starting the gateway."
    )]
    Config(ConfigArgs),
}

/// `spire config <subcommand>`
#[derive(Args, Debug, Clone)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ConfigCommand {
    #[command(
        about = "Print the fully expanded configuration",
        long_about = "Parse the configuration file and print the result to stdout. \
                      Equivalent to 'nginx -T'.",
        after_help = "\
EXAMPLES:
    spire config expand -c config.yaml
        Expand config.yaml and print it

    spire -f config.yaml config expand
        Use the global -f/--config instead of -c

OUTPUT:
    The parsed configuration after defaults are applied, with secrets included:
    this is a local debugging command, and redacting them would defeat diffing.
    Invalid configurations are rejected with the same errors as 'spire validate'."
    )]
    Expand(ExpandArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ExpandArgs {
    /// Configuration file to expand (defaults to the global -f/--config)
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct ConvertArgs {
    #[arg(required = true, value_name = "INPUT_FILE")]
    pub input_file: PathBuf,

    #[arg(short = 'o', long, value_name = "OUTPUT_FILE")]
    pub output_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = InputFormat::Openapi)]
    pub format: InputFormat,
}

#[derive(Args, Debug, Clone)]
pub struct ExamplesArgs {
    /// List all available examples
    #[arg(short, long)]
    pub list: bool,

    /// Display a specific example by name (without .yaml extension)
    #[arg(value_name = "EXAMPLE_NAME")]
    pub name: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct ValidateArgs {
    /// Configuration file to validate
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<String>,

    /// Show detailed validation output
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ReloadArgs {
    /// Process ID of the running Spire instance to signal
    #[arg(long, value_name = "PID")]
    pub pid: u32,
}

#[derive(Clone)]
pub struct SharedConfig {
    /// `RwLock`, not `Mutex`: request routing only needs to *read* the
    /// configuration, so thousands of in-flight requests can proceed in
    /// parallel. Only reloads and health-check state updates take the write
    /// lock.
    pub shared_data: Arc<RwLock<AppConfig>>,
    /// File the configuration was loaded from; control plane edits are
    /// persisted back to exactly this path.
    pub config_path: Arc<PathBuf>,
}
impl SharedConfig {
    #[allow(dead_code)] // convenience constructor used by tests
    pub fn from_app_config(app_config: AppConfig) -> Self {
        Self::from_app_config_with_path(app_config, PathBuf::from("config.yaml"))
    }

    pub fn from_app_config_with_path(app_config: AppConfig, path: impl Into<PathBuf>) -> Self {
        Self {
            shared_data: Arc::new(RwLock::new(app_config)),
            config_path: Arc::new(path.into()),
        }
    }
}
