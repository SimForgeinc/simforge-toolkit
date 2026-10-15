//! Engine-free clap subcommands and `TemplateOp` JSON derived from `sdk/contracts/authoring-operations.v1.json`.

use std::sync::LazyLock;

use clap::builder::{PossibleValuesParser, ValueParser};
use clap::error::ErrorKind;
use clap::{Arg, ArgAction, ArgGroup, ArgMatches, Command, FromArgMatches, Subcommand};
use serde_json::{json, Map, Value};

use crate::contract::CliError;

pub const CATALOGUE_JSON: &str = include_str!("../../../../contracts/authoring-operations.v1.json");

/// Separates the operations of one transaction on the command line.
pub const OPERATION_SEPARATOR: &str = "--";
const MORE: &str = "more_operations";

static CATALOGUE: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(CATALOGUE_JSON).expect("authoring-operations.v1.json is valid JSON")
});

static SPECS: LazyLock<Vec<OpSpec>> = LazyLock::new(|| {
    CATALOGUE["operations"]
        .as_array()
        .expect("catalogue operations")
        .iter()
        .map(op_spec)
        .collect()
});

pub fn catalogue() -> &'static Value {
    &CATALOGUE
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Text,
    Number,
    Integer,
    /// A required boolean: `--flag true|false`.
    Boolean,
    /// An optional boolean defaulting to false: present means true.
    Switch,
    Choice(Vec<&'static str>),
    /// An array of objects, one repeated `--flag A,B` per item.
    Tuples(Vec<&'static str>),
}

#[derive(Debug)]
pub struct FlagSpec {
    pub property: &'static str,
    pub flag: String,
    pub kind: Kind,
    pub required: bool,
    pub schema: &'static Value,
    value_name: String,
    help: String,
}

#[derive(Debug)]
struct GroupSpec {
    id: String,
    /// Each branch's properties; the first of each is the group member.
    branches: Vec<Vec<&'static str>>,
    exclusive: bool,
}

#[derive(Debug)]
pub struct OpSpec {
    pub id: &'static str,
    pub command: String,
    pub summary: &'static str,
    pub entry: &'static Value,
    pub flags: Vec<FlagSpec>,
    groups: Vec<GroupSpec>,
}

/// `laneRsl` -> `lane-rsl`.
pub fn kebab(camel: &str) -> String {
    let mut out = String::with_capacity(camel.len() + 4);
    for c in camel.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn screaming(camel: &str) -> String {
    kebab(camel).replace('-', "_").to_ascii_uppercase()
}

fn strs(value: Option<&'static Value>) -> Vec<&'static str> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn bounds_text(schema: &Value) -> Option<String> {
    let min = schema
        .get("minimum")
        .or_else(|| schema.get("exclusiveMinimum"));
    let open = schema.get("exclusiveMinimum").is_some();
    let max = schema.get("maximum");
    let range = match (min, max) {
        (Some(a), Some(b)) => Some(format!("{a}..{b}")),
        (Some(a), None) if open => Some(format!("> {a}")),
        (Some(a), None) => Some(format!(">= {a}")),
        (None, Some(b)) => Some(format!("<= {b}")),
        (None, None) => None,
    };
    let unit = schema.get("x-unit").and_then(Value::as_str);
    match (range, unit) {
        (Some(r), Some(u)) => Some(format!("{r} {u}")),
        (Some(r), None) => Some(r),
        (None, Some(u)) => Some(u.to_owned()),
        (None, None) => None,
    }
}

fn flag_spec(property: &'static str, schema: &'static Value, required: bool) -> FlagSpec {
    let ty = schema
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("string");
    let choices = strs(schema.get("enum"));
    let (kind, flag, value_name) = if !choices.is_empty() {
        (Kind::Choice(choices), kebab(property), screaming(property))
    } else {
        match ty {
            "number" => (Kind::Number, kebab(property), "N".to_owned()),
            "integer" => (Kind::Integer, kebab(property), "N".to_owned()),
            "boolean" if !required && schema.get("default") == Some(&Value::Bool(false)) => {
                (Kind::Switch, kebab(property), String::new())
            }
            "boolean" => (Kind::Boolean, kebab(property), "BOOL".to_owned()),
            "array" => {
                let fields = strs(schema.pointer("/items/required"));
                let singular = property.strip_suffix('s').unwrap_or(property);
                let name = fields
                    .iter()
                    .map(|f| f.to_ascii_uppercase())
                    .collect::<Vec<_>>();
                (Kind::Tuples(fields), kebab(singular), name.join(","))
            }
            _ => (Kind::Text, kebab(property), screaming(property)),
        }
    };
    let mut help = schema
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if let Some(bounds) = bounds_text(schema) {
        help.push_str(&format!(" [{bounds}]"));
    }
    if let Some(default) = schema.get("default").filter(|_| kind != Kind::Switch) {
        help.push_str(&format!(" (default {default})"));
    }
    if let Kind::Tuples(_) = kind {
        let min = schema.get("minItems").and_then(Value::as_u64).unwrap_or(1);
        help.push_str(&format!(" Repeat once per item, at least {min}."));
    }
    FlagSpec {
        property,
        flag,
        kind,
        required,
        schema,
        value_name,
        help,
    }
}

fn op_spec(entry: &'static Value) -> OpSpec {
    let id = entry["id"].as_str().expect("catalogue operation id");
    let input = &entry["inputSchema"];
    let required = strs(input.get("required"));
    let flags = input["properties"]
        .as_object()
        .expect("inputSchema.properties")
        .iter()
        .filter(|(name, _)| name.as_str() != "type")
        .map(|(name, schema)| flag_spec(name.as_str(), schema, required.contains(&name.as_str())))
        .collect();
    let groups = [("oneOf", true), ("anyOf", false)]
        .into_iter()
        .filter_map(|(key, exclusive)| {
            let branches: Vec<Vec<&'static str>> = input
                .get(key)?
                .as_array()?
                .iter()
                .map(|branch| strs(branch.get("required")))
                .collect();
            Some(GroupSpec {
                id: format!("{id}-{key}"),
                branches,
                exclusive,
            })
        })
        .collect();
    OpSpec {
        id,
        command: kebab(id),
        summary: entry["summary"].as_str().unwrap_or_default(),
        entry,
        flags,
        groups,
    }
}

pub fn specs() -> &'static [OpSpec] {
    &SPECS
}

/// An operation by its id (`addActor`) or command name (`add-actor`).
pub fn spec(name: &str) -> Option<&'static OpSpec> {
    specs().iter().find(|s| s.id == name || s.command == name)
}

fn number_parser(schema: &'static Value, integer: bool) -> ValueParser {
    let min = schema.get("minimum").and_then(Value::as_f64);
    let above = schema.get("exclusiveMinimum").and_then(Value::as_f64);
    let max = schema.get("maximum").and_then(Value::as_f64);
    let check = move |v: f64| -> Result<f64, String> {
        if !v.is_finite() {
            return Err("expected a finite number".into());
        }
        if min.is_some_and(|m| v < m) || above.is_some_and(|m| v <= m) || max.is_some_and(|m| v > m)
        {
            return Err(format!(
                "out of range ({})",
                bounds_text(schema).unwrap_or_default()
            ));
        }
        Ok(v)
    };
    if integer {
        ValueParser::new(move |s: &str| -> Result<i64, String> {
            let v: i64 = s
                .trim()
                .parse()
                .map_err(|_| "expected an integer".to_owned())?;
            check(v as f64).map(|_| v)
        })
    } else {
        ValueParser::new(move |s: &str| -> Result<f64, String> {
            check(
                s.trim()
                    .parse()
                    .map_err(|_| "expected a number".to_owned())?,
            )
        })
    }
}

fn tuple_parser(fields: Vec<&'static str>) -> ValueParser {
    ValueParser::new(move |s: &str| -> Result<Value, String> {
        let parts: Vec<&str> = s.split(',').collect();
        if parts.len() != fields.len() {
            return Err(format!("expected {} comma-separated numbers", fields.len()));
        }
        let mut item = Map::new();
        for (field, part) in fields.iter().zip(parts) {
            let v: f64 = part
                .trim()
                .parse()
                .ok()
                .filter(|v: &f64| v.is_finite())
                .ok_or_else(|| format!("{field}: expected a finite number"))?;
            item.insert((*field).to_owned(), json!(v));
        }
        Ok(Value::Object(item))
    })
}

impl FlagSpec {
    fn arg(&'static self) -> Arg {
        let arg = Arg::new(self.property)
            .long(self.flag.as_str())
            .help(self.help.as_str())
            .required(self.required);
        match &self.kind {
            Kind::Switch => arg.action(ArgAction::SetTrue),
            Kind::Text => arg.value_name(self.value_name.as_str()),
            Kind::Number => arg
                .value_name(self.value_name.as_str())
                .allow_negative_numbers(true)
                .value_parser(number_parser(self.schema, false)),
            Kind::Integer => arg
                .value_name(self.value_name.as_str())
                .allow_negative_numbers(true)
                .value_parser(number_parser(self.schema, true)),
            Kind::Boolean => arg
                .value_name(self.value_name.as_str())
                .value_parser(PossibleValuesParser::new(["true", "false"])),
            Kind::Choice(values) => arg
                .value_name(self.value_name.as_str())
                .value_parser(PossibleValuesParser::new(values.iter().copied())),
            Kind::Tuples(fields) => arg
                .value_name(self.value_name.as_str())
                .action(ArgAction::Append)
                .allow_hyphen_values(true)
                .value_parser(tuple_parser(fields.clone())),
        }
    }

    fn value(&self, matches: &ArgMatches) -> Result<Option<Value>, CliError> {
        let id = self.property;
        Ok(match &self.kind {
            Kind::Switch => matches.get_flag(id).then_some(Value::Bool(true)),
            Kind::Number => matches.get_one::<f64>(id).map(|v| json!(v)),
            Kind::Integer => matches.get_one::<i64>(id).map(|v| json!(v)),
            Kind::Boolean => matches
                .get_one::<String>(id)
                .map(|v| Value::Bool(v == "true")),
            Kind::Text | Kind::Choice(_) => matches.get_one::<String>(id).map(|v| json!(v)),
            Kind::Tuples(_) => {
                let items: Vec<Value> = matches
                    .get_many::<Value>(id)
                    .map(|v| v.cloned().collect())
                    .unwrap_or_default();
                let min = self
                    .schema
                    .get("minItems")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if (items.len() as u64) < min && (self.required || !items.is_empty()) {
                    return Err(CliError::new(
                        "bad_value",
                        format!("--{} needs at least {min} values", self.flag),
                    )
                    .with_path(format!("--{}", self.flag)));
                }
                (!items.is_empty()).then_some(Value::Array(items))
            }
        })
    }
}

impl OpSpec {
    fn flag_of(&self, property: &str) -> Option<&FlagSpec> {
        self.flags.iter().find(|f| f.property == property)
    }

    /// The clap subcommand for this operation.
    pub fn command(&'static self) -> Command {
        let mut cmd = Command::new(self.command.as_str())
            .about(self.summary)
            .args(self.flags.iter().map(FlagSpec::arg))
            .arg(
                Arg::new(MORE)
                    .num_args(0..)
                    .last(true)
                    .allow_hyphen_values(true)
                    .value_name("OPERATION")
                    .help("Further operations of the same transaction, each after `--`."),
            );
        for group in &self.groups {
            let leaders: Vec<&'static str> = group
                .branches
                .iter()
                .filter_map(|b| b.first().copied())
                .collect();
            cmd = cmd.group(
                ArgGroup::new(group.id.as_str())
                    .args(leaders)
                    .required(true)
                    .multiple(!group.exclusive),
            );
            for (i, branch) in group.branches.iter().enumerate() {
                for &member in branch {
                    let mut arg = cmd
                        .get_arguments()
                        .find(|a| a.get_id() == member)
                        .cloned()
                        .expect("branch member is a property");
                    for &other in branch.iter().filter(|&&o| o != member) {
                        arg = arg.requires(other);
                    }
                    if group.exclusive {
                        for (j, rival) in group.branches.iter().enumerate() {
                            if j != i {
                                arg = arg.conflicts_with_all(
                                    rival.iter().copied().filter(|r| !branch.contains(r)),
                                );
                            }
                        }
                    }
                    cmd = cmd.mut_arg(member, |_| arg);
                }
            }
        }
        cmd
    }

    /// The operation JSON from this subcommand's matches.
    pub fn operation(&self, matches: &ArgMatches) -> Result<Value, CliError> {
        let mut op = Map::new();
        op.insert("type".into(), json!(self.id));
        for flag in &self.flags {
            if let Some(value) = flag.value(matches)? {
                op.insert(flag.property.to_owned(), value);
            }
        }
        Ok(Value::Object(op))
    }

    /// The catalogue entry with its command name and derived flags.
    pub fn show(&self) -> Value {
        let mut out = self.entry.clone();
        out["command"] = json!(self.command);
        out["flags"] = Value::Array(
            self.flags
                .iter()
                .map(|f| {
                    let mut flag = json!({
                        "flag": format!("--{}", f.flag),
                        "property": f.property,
                        "type": match &f.kind {
                            Kind::Text => "string",
                            Kind::Number => "number",
                            Kind::Integer => "integer",
                            Kind::Boolean => "bool-value",
                            Kind::Switch => "bool",
                            Kind::Choice(_) => "enum",
                            Kind::Tuples(_) => "tuple",
                        },
                        "required": f.required,
                        "repeatable": matches!(f.kind, Kind::Tuples(_)),
                        "description": f.help,
                    });
                    if !f.value_name.is_empty() {
                        flag["valueName"] = json!(f.value_name);
                    }
                    for key in [
                        "enum",
                        "minimum",
                        "maximum",
                        "exclusiveMinimum",
                        "default",
                        "minItems",
                        "x-unit",
                    ] {
                        if let Some(v) = f.schema.get(key) {
                            flag[key.trim_start_matches("x-")] = v.clone();
                        }
                    }
                    flag
                })
                .collect(),
        );
        for group in &self.groups {
            let key = if group.exclusive {
                "requiredOneOf"
            } else {
                "requiredAnyOf"
            };
            out[key] = json!(group
                .branches
                .iter()
                .map(|b| b
                    .iter()
                    .filter_map(|p| self.flag_of(p))
                    .map(|f| format!("--{}", f.flag))
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>());
        }
        out
    }
}

fn operations_command() -> Command {
    Operations::augment_subcommands(Command::new("operation").no_binary_name(true))
        .subcommand_required(true)
}

/// Operations from argv tokens starting at an operation name, chained with `--`.
pub fn parse_operations(tokens: &[String]) -> Result<Vec<Value>, CliError> {
    let mut out = Vec::new();
    for segment in tokens.split(|t| t == OPERATION_SEPARATOR) {
        if segment.is_empty() {
            return Err(CliError::new(
                "missing_command",
                "an empty operation between `--` separators",
            )
            .with_detail(
                json!({ "known": specs().iter().map(|s| s.command.as_str()).collect::<Vec<_>>() }),
            ));
        }
        let matches = operations_command()
            .try_get_matches_from(segment)
            .map_err(|e| crate::parse_error(&e, segment))?;
        let (name, sub) = matches.subcommand().expect("subcommand required");
        out.push(spec(name).expect("known subcommand").operation(sub)?);
    }
    Ok(out)
}

/// One transaction's operations as a clap subcommand, further ones after `--`.
#[derive(Debug, Clone, PartialEq)]
pub struct Operations(pub Vec<Value>);

fn clap_error(error: CliError) -> clap::Error {
    clap::Error::raw(ErrorKind::ValueValidation, format!("{}\n", error.reason))
}

impl FromArgMatches for Operations {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        let (name, sub) = matches
            .subcommand()
            .ok_or_else(|| clap::Error::new(ErrorKind::MissingSubcommand))?;
        let spec = spec(name).ok_or_else(|| clap::Error::new(ErrorKind::InvalidSubcommand))?;
        let mut operations = vec![spec.operation(sub).map_err(clap_error)?];
        let more: Vec<String> = sub
            .get_many::<String>(MORE)
            .map(|v| v.cloned().collect())
            .unwrap_or_default();
        if !more.is_empty() {
            operations.extend(parse_operations(&more).map_err(clap_error)?);
        }
        Ok(Self(operations))
    }

    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Subcommand for Operations {
    fn augment_subcommands(cmd: Command) -> Command {
        specs()
            .iter()
            .fold(cmd, |cmd, spec| cmd.subcommand(spec.command()))
    }

    fn augment_subcommands_for_update(cmd: Command) -> Command {
        Self::augment_subcommands(cmd)
    }

    fn has_subcommand(name: &str) -> bool {
        specs().iter().any(|s| s.command == name)
    }
}

/// `ops list`: every operation, its command and what it needs.
pub fn list_value() -> Value {
    json!({
        "schema": catalogue()["schema"],
        "operations": specs().iter().map(|s| json!({
            "id": s.id,
            "command": s.command,
            "summary": s.summary,
            "effectCategory": s.entry["effectCategory"],
            "requiredContext": s.entry["requiredContext"],
        })).collect::<Vec<_>>(),
    })
}

/// `ops show <id>`: one operation's catalogue entry with its derived flags.
pub fn show_value(name: &str) -> Result<Value, CliError> {
    spec(name).map(OpSpec::show).ok_or_else(|| {
        CliError::new(
            "unknown_operation",
            format!("no authoring operation `{name}`"),
        )
        .with_path(name.to_owned())
        .with_detail(
            json!({ "known": specs().iter().map(|s| s.command.as_str()).collect::<Vec<_>>() }),
        )
    })
}

/// The document transaction request; `label` defaults to the summaries.
pub fn transaction_value(
    base_version: u64,
    base_digest: &str,
    label: Option<&str>,
    operations: Vec<Value>,
) -> Value {
    let label = label.map(str::to_owned).unwrap_or_else(|| {
        operations
            .iter()
            .filter_map(|op| {
                op["type"]
                    .as_str()
                    .and_then(spec)
                    .map(|s| s.summary.trim_end_matches('.'))
            })
            .collect::<Vec<_>>()
            .join("; ")
    });
    json!({
        "baseVersion": base_version,
        "baseDigest": base_digest,
        "label": label,
        "operations": operations,
    })
}

#[cfg(test)]
#[path = "authoring_ops_tests.rs"]
mod tests;
