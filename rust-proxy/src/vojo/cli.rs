use clap::Parser;

use super::app_config::AppConfig;
use clap::Args;
use clap::Subcommand;
use clap::ValueEnum;
use std::path::PathBuf;
use std::sync::Arc;

use std::sync::Mutex;
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

    spire convert openapi.yaml -o config.yaml
        Convert OpenAPI/Swagger spec to gateway config

    spire reload config.yaml
        Reload configuration without restarting

    spire query
        Query current configuration from control plane

COMMANDS:
    convert (conv)   Convert OpenAPI/Swagger files to gateway configuration
    examples (ex)    List and display configuration examples
    query (q)        Query current configuration from control plane
    reload (rel)     Reload configuration from a file
    validate (val)   Validate a configuration file

CONFIGURATION:
    Configuration files use YAML format. See 'spire examples' for available examples.
    Default config file: config.yaml
    Use 'spire validate' to check your config before starting

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
        List all 21 available configuration examples with descriptions

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
        forward_ip_examples, request_headers, http_cors, jwt_auth, middle_wares

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
        about = "Reload configuration from a file",
        long_about = "Reload the gateway configuration by sending a new config file to the control plane API.",
        after_help = "\
EXAMPLES:
    spire reload config.yaml
        Reload configuration from config.yaml

    spire reload new_config.yaml --port 8081
        Reload from specific config file and connect to control plane on port 8081

VALIDATION:
    The reload command validates that the new configuration has the exact same set of listen ports
    as the current configuration. Both the number of ports and the port values must match exactly.

    This restriction ensures that the gateway continues listening on the same ports and provides
    a smooth configuration update without interrupting existing connections."
    )]
    Reload(ReloadArgs),
    #[command(
        visible_alias = "q",
        about = "Query current configuration from control plane",
        long_about = "Fetch and display the current gateway configuration from the control plane API.",
        after_help = "\
EXAMPLES:
    spire query
        Query current configuration using default host and port

    spire query --host 192.168.1.100 --port 9090
        Query from specific control plane address

OUTPUT:
    The configuration is displayed in YAML format, showing all current settings including:
    - Admin port
    - Log level
    - API service configurations
    - Route configurations
    - Health check settings
    - And other configuration options"
    )]
    Query(QueryArgs),
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
    /// Configuration file to load
    #[arg(value_name = "FILE")]
    pub config: String,

    /// Control plane host
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Control plane port
    #[arg(short, long, default_value = "8081")]
    pub port: u16,
}

#[derive(Args, Debug, Clone)]
pub struct QueryArgs {
    /// Control plane host
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Control plane port
    #[arg(short, long, default_value = "8081")]
    pub port: u16,

    /// Output format (yaml or json)
    #[arg(short, long, default_value = "yaml")]
    pub format: String,
}

#[derive(Clone)]
pub struct SharedConfig {
    pub shared_data: Arc<Mutex<AppConfig>>,
}
impl SharedConfig {
    pub fn from_app_config(app_config: AppConfig) -> Self {
        Self {
            shared_data: Arc::new(Mutex::new(app_config)),
        }
    }
}
