use clap::Clap;
use colored::Colorize;
use regex::Regex;
use serde::{Deserialize, Deserializer};
use std::fs::File;
use std::io::{self, BufReader, Write};
use std::process::{Command, Stdio};
use std::string::FromUtf8Error;
use thiserror::Error;

const STDERR_UTF8_MESSAGE: &'static str = "stderr contained malformed UTF-8 text";
const STDOUT_UTF8_MESSAGE: &'static str = "stdout contained malformed UTF-8 text";

#[derive(Clap, Debug, Clone, Hash, PartialEq, Eq)]
struct Options {
    /// The path to the autograding configuration
    #[clap(short, long, default_value = "./.github/classroom/autograding.json")]
    config: String,
    /// Removes \r from test inputs and outputs
    #[clap(short, long)]
    strip_crlf: bool,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ConfigRoot {
    tests: Vec<TestCase>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TestCase {
    name: String,
    #[serde(deserialize_with = "deserialize_excluding_empty_strings")]
    setup: Option<String>,
    run: String,
    #[serde(deserialize_with = "deserialize_excluding_empty_strings")]
    input: Option<String>,
    #[serde(deserialize_with = "deserialize_excluding_empty_strings")]
    output: Option<String>,
    comparison: Option<Comparison>,
    timeout: Option<u16>, // Unused
    points: Option<u16>,
}

fn deserialize_excluding_empty_strings<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    if s.is_empty() {
        Ok(None)
    } else {
        Ok(Some(s))
    }
}

#[derive(Deserialize, Debug, Copy, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
enum Comparison {
    Included,
    Exact,
    Regex,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TestOutcome {
    success: bool,
    stdout: String,
}

#[derive(Debug, Error)]
enum TestFailure {
    #[error("{0}")]
    Stderr(String),
    #[error("{0}")]
    Message(String),
    #[error("{reason}\n{error}")]
    Io {
        error: io::Error,
        reason: &'static str,
    },
    #[error("{reason}\n{error}")]
    Utf8 {
        error: FromUtf8Error,
        reason: &'static str,
    },
    #[error("{error}\n{reason}")]
    Regex {
        error: regex::Error,
        reason: &'static str,
    },
}

impl TestFailure {
    fn print(&self, test_name: &str) {
        match self {
            TestFailure::Stderr(stderr) => {
                println!("{}❌ {}", stderr, test_name.red());
            }
            TestFailure::Utf8 { error, reason } => {
                // If we can't print these bytes at this point,
                // it's a lost cause. ☠️
                let _ = std::io::stdout().write(&error.as_bytes());
                println!(
                    "{}\n{}\n❌ {}",
                    reason.red(),
                    error.to_string().red(),
                    test_name.red()
                );
            }
            other => {
                println!("{}\n❌ {}", other.to_string().red(), test_name.red());
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options: Options = Options::parse();
    let file = File::open(options.config)?;
    let reader = BufReader::new(file);
    let config = {
        let mut config: ConfigRoot = serde_json::from_reader(reader)?;
        if options.strip_crlf {
            for test in config.tests.iter_mut() {
                test.input = test.input.take().map(|input| strip_crlf(&input));
                test.output = test.output.take().map(|output| strip_crlf(&output));
            }
        }
        config
    };

    let total_points = config
        .tests
        .iter()
        .filter_map(|test| test.points)
        .reduce(|a, b| a + b)
        .unwrap_or(0);

    let mut points = 0u16;
    let mut all_succeeded = true;

    for test in config.tests {
        let pass = set_up_and_run_test(&test);
        if pass {
            if let Some(test_points) = test.points {
                points += test_points;
            }
        } else {
            all_succeeded = false;
        }
        println!("\n");
    }

    if all_succeeded {
        println!(
            "{}\n✨🌟💖💎🦄💎💖🌟✨🌟💖💎🦄💎💖🌟✨",
            "All tests pass".green()
        );
    }
    println!("Points {}/{}", points, total_points);
    Ok(())
}

fn set_up_and_run_test(test: &TestCase) -> bool {
    println!("📝 {}", test.name);
    if let Some(setup) = &test.setup {
        match set_up_test(&setup) {
            Ok(stdout) => {
                print!("{}", stdout);
            }
            Err(error) => {
                error.print(&test.name);
                return false;
            }
        }
    }
    match run_test(&test) {
        Ok(outcome) => {
            if outcome.success {
                println!("{}✅ {}", outcome.stdout, test.name.green())
            } else {
                println!("{}❌ {}", outcome.stdout, test.name.red())
            }
            outcome.success
        }
        Err(error) => {
            error.print(&test.name);
            false
        }
    }
}

fn set_up_test(setup_command: &str) -> Result<String, TestFailure> {
    let output = Command::new(setup_command)
        .output()
        .map_err(|error| TestFailure::Io {
            error,
            reason: "Failed to run test setup command",
        })?;
    if output.status.success() {
        let stdout = String::from_utf8(output.stdout).map_err(|error| TestFailure::Utf8 {
            error,
            reason: STDOUT_UTF8_MESSAGE,
        })?;
        Ok(stdout)
    } else {
        let stderr = String::from_utf8(output.stderr).map_err(|error| TestFailure::Utf8 {
            error,
            reason: STDERR_UTF8_MESSAGE,
        })?;
        Err(TestFailure::Stderr(stderr))
    }
}

fn run_test(test: &TestCase) -> Result<TestOutcome, TestFailure> {
    let mut command = Command::new("bash")
        .args(&["-c", &test.run])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| TestFailure::Io {
            error,
            reason: "Failed to start bash with the test run command",
        })?;

    if let Some(input) = &test.input {
        let stdin = command.stdin.as_mut().ok_or(TestFailure::Message(
            "Could not get a handle to stdin".to_string(),
        ))?;
        stdin
            .write_all(input.as_bytes())
            .map_err(|error| TestFailure::Io {
                error,
                reason: "Failed to pipe input to the running test process",
            })?;
    } // Stdin drops and finishes input

    let output = command
        .wait_with_output()
        .map_err(|error| TestFailure::Io {
            error,
            reason: "Failed to run the test to completion",
        })?;
    if output.status.success() {
        let stdout = String::from_utf8(output.stdout).map_err(|error| TestFailure::Utf8 {
            error,
            reason: STDOUT_UTF8_MESSAGE,
        })?;
        let success = if let Some(expected_output) = &test.output {
            if let Some(comparison) = &test.comparison {
                match comparison {
                    Comparison::Included => stdout.contains(expected_output),
                    Comparison::Exact => stdout.eq(expected_output),
                    Comparison::Regex => {
                        let re =
                            Regex::new(&expected_output).map_err(|error| TestFailure::Regex {
                                error,
                                reason: "Failed to parse regex for output comparison",
                            })?;
                        re.is_match(&stdout)
                    }
                }
            } else {
                true
            }
        } else {
            true
        };
        Ok(TestOutcome { success, stdout })
    } else {
        let stderr = String::from_utf8(output.stderr).map_err(|error| TestFailure::Utf8 {
            error,
            reason: STDERR_UTF8_MESSAGE,
        })?;
        Err(TestFailure::Stderr(stderr))
    }
}

fn strip_crlf(to_strip: &str) -> String {
    let mut out = String::with_capacity(to_strip.len());
    let mut iter = to_strip.chars();
    while let Some(next) = iter.next() {
        match next {
            '\r' => {
                // Do nothing
            }
            c => out.push(c),
        }
    }
    out
}
