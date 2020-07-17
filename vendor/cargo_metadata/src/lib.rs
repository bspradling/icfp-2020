#![deny(missing_docs)]
//! Structured access to the output of `cargo metadata` and `cargo --message-format=json`.
//! Usually used from within a `cargo-*` executable
//!
//! See the [cargo book](https://doc.rust-lang.org/cargo/index.html) for
//! details on cargo itself.
//!
//! ## Examples
//!
//! ```rust
//! # extern crate cargo_metadata;
//! # use std::path::Path;
//! let mut args = std::env::args().skip_while(|val| !val.starts_with("--manifest-path"));
//!
//! let mut cmd = cargo_metadata::MetadataCommand::new();
//! let manifest_path = match args.next() {
//!     Some(ref p) if p == "--manifest-path" => {
//!         cmd.manifest_path(args.next().unwrap());
//!     }
//!     Some(p) => {
//!         cmd.manifest_path(p.trim_start_matches("--manifest-path="));
//!     }
//!     None => {}
//! };
//!
//! let _metadata = cmd.exec().unwrap();
//! ```
//!
//! Pass features flags
//!
//! ```rust
//! # // This should be kept in sync with the equivalent example in the readme.
//! # extern crate cargo_metadata;
//! # use std::path::Path;
//! # fn main() {
//! use cargo_metadata::{MetadataCommand, CargoOpt};
//!
//! let _metadata = MetadataCommand::new()
//!     .manifest_path("./Cargo.toml")
//!     .features(CargoOpt::AllFeatures)
//!     .exec()
//!     .unwrap();
//! # }
//! ```
//!
//! Parse message-format output:
//!
//! ```
//! # extern crate cargo_metadata;
//! use std::process::{Stdio, Command};
//! use cargo_metadata::Message;
//!
//! let mut command = Command::new("cargo")
//!     .args(&["build", "--message-format=json"])
//!     .stdout(Stdio::piped())
//!     .spawn()
//!     .unwrap();
//!
//! let reader = std::io::BufReader::new(command.stdout.take().unwrap());
//! for message in cargo_metadata::Message::parse_stream(reader) {
//!     match message.unwrap() {
//!         Message::CompilerMessage(msg) => {
//!             println!("{:?}", msg);
//!         },
//!         Message::CompilerArtifact(artifact) => {
//!             println!("{:?}", artifact);
//!         },
//!         Message::BuildScriptExecuted(script) => {
//!             println!("{:?}", script);
//!         },
//!         Message::BuildFinished(finished) => {
//!             println!("{:?}", finished);
//!         },
//!         _ => () // Unknown message
//!     }
//! }
//!
//! let output = command.wait().expect("Couldn't get cargo's exit status");
//! ```

extern crate semver;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;

use std::collections::HashMap;
use std::env;
use std::fmt;
use std::path::PathBuf;
use std::process::Command;
use std::str::from_utf8;

use semver::Version;

pub use dependency::{Dependency, DependencyKind};
use diagnostic::Diagnostic;
pub use errors::{Error, Result};
pub use messages::{
    Artifact, ArtifactProfile, BuildFinished, BuildScript, CompilerMessage, Message, MessageIter
};
#[allow(deprecated)]
pub use messages::parse_messages;

mod dependency;
pub mod diagnostic;
mod errors;
mod messages;

/// An "opaque" identifier for a package.
/// It is possible to inspect the `repr` field, if the need arises, but its
/// precise format is an implementation detail and is subject to change.
///
/// `Metadata` can be indexed by `PackageId`.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct PackageId {
    /// The underlying string representation of id.
    pub repr: String,
}

impl std::fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.repr, f)
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
/// Starting point for metadata returned by `cargo metadata`
pub struct Metadata {
    /// A list of all crates referenced by this crate (and the crate itself)
    pub packages: Vec<Package>,
    /// A list of all workspace members
    pub workspace_members: Vec<PackageId>,
    /// Dependencies graph
    pub resolve: Option<Resolve>,
    /// Workspace root
    pub workspace_root: PathBuf,
    /// Build directory
    pub target_directory: PathBuf,
    version: usize,
    #[doc(hidden)]
    #[serde(skip)]
    __do_not_match_exhaustively: (),
}

impl<'a> std::ops::Index<&'a PackageId> for Metadata {
    type Output = Package;

    fn index(&self, idx: &'a PackageId) -> &Package {
        self.packages
            .iter()
            .find(|p| p.id == *idx)
            .unwrap_or_else(|| panic!("no package with this id: {:?}", idx))
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
/// A dependency graph
pub struct Resolve {
    /// Nodes in a dependencies graph
    pub nodes: Vec<Node>,

    /// The crate for which the metadata was read.
    pub root: Option<PackageId>,
    #[doc(hidden)]
    #[serde(skip)]
    __do_not_match_exhaustively: (),
}

#[derive(Clone, Serialize, Deserialize, Debug)]
/// A node in a dependencies graph
pub struct Node {
    /// An opaque identifier for a package
    pub id: PackageId,
    /// Dependencies in a structured format.
    ///
    /// `deps` handles renamed dependencies whereas `dependencies` does not.
    #[serde(default)]
    pub deps: Vec<NodeDep>,

    /// List of opaque identifiers for this node's dependencies.
    /// It doesn't support renamed dependencies. See `deps`.
    pub dependencies: Vec<PackageId>,

    /// Features enabled on the crate
    #[serde(default)]
    pub features: Vec<String>,
    #[doc(hidden)]
    #[serde(skip)]
    __do_not_match_exhaustively: (),
}

#[derive(Clone, Serialize, Deserialize, Debug)]
/// A dependency in a node
pub struct NodeDep {
    /// The name of the dependency's library target.
    /// If the crate was renamed, it is the new name.
    pub name: String,
    /// Package ID (opaque unique identifier)
    pub pkg: PackageId,
    /// The kinds of dependencies.
    ///
    /// This field was added in Rust 1.41.
    #[serde(default)]
    pub dep_kinds: Vec<DepKindInfo>,
    #[doc(hidden)]
    #[serde(skip)]
    __do_not_match_exhaustively: (),
}

#[derive(Clone, Serialize, Deserialize, Debug)]
/// Information about a dependency kind.
pub struct DepKindInfo {
    /// The kind of dependency.
    #[serde(deserialize_with = "dependency::parse_dependency_kind")]
    pub kind: DependencyKind,
    /// The target platform for the dependency.
    ///
    /// This is `None` if it is not a target dependency.
    ///
    /// Use the [`Display`] trait to access the contents.
    ///
    /// By default all platform dependencies are included in the resolve
    /// graph. Use Cargo's `--filter-platform` flag if you only want to
    /// include dependencies for a specific platform.
    ///
    /// [`Display`]: https://doc.rust-lang.org/std/fmt/trait.Display.html
    pub target: Option<dependency::Platform>,
    #[doc(hidden)]
    #[serde(skip)]
    __do_not_match_exhaustively: (),
}

#[derive(Clone, Serialize, Deserialize, Debug)]
/// A crate
pub struct Package {
    /// Name as given in the `Cargo.toml`
    pub name: String,
    /// Version given in the `Cargo.toml`
    pub version: Version,
    /// Authors given in the `Cargo.toml`
    #[serde(default)]
    pub authors: Vec<String>,
    /// An opaque identifier for a package
    pub id: PackageId,
    /// The source of the package, e.g.
    /// crates.io or `None` for local projects.
    pub source: Option<Source>,
    /// Description as given in the `Cargo.toml`
    pub description: Option<String>,
    /// List of dependencies of this particular package
    pub dependencies: Vec<Dependency>,
    /// License as given in the `Cargo.toml`
    pub license: Option<String>,
    /// If the package is using a nonstandard license, this key may be specified instead of
    /// `license`, and must point to a file relative to the manifest.
    pub license_file: Option<PathBuf>,
    /// Targets provided by the crate (lib, bin, example, test, ...)
    pub targets: Vec<Target>,
    /// Features provided by the crate, mapped to the features required by that feature.
    pub features: HashMap<String, Vec<String>>,
    /// Path containing the `Cargo.toml`
    pub manifest_path: PathBuf,
    /// Categories as given in the `Cargo.toml`
    #[serde(default)]
    pub categories: Vec<String>,
    /// Keywords as given in the `Cargo.toml`
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Readme as given in the `Cargo.toml`
    pub readme: Option<PathBuf>,
    /// Repository as given in the `Cargo.toml`
    // can't use `url::Url` because that requires a more recent stable compiler
    pub repository: Option<String>,
    /// Default Rust edition for the package
    ///
    /// Beware that individual targets may specify their own edition in
    /// [`Target::edition`](struct.Target.html#structfield.edition).
    #[serde(default = "edition_default")]
    pub edition: String,
    /// Contents of the free form package.metadata section
    ///
    /// This contents can be serialized to a struct using serde:
    ///
    /// ```rust
    /// #[macro_use]
    /// extern crate serde_json;
    /// #[macro_use]
    /// extern crate serde_derive;
    ///
    /// #[derive(Debug, Deserialize)]
    /// struct SomePackageMetadata {
    ///     some_value: i32,
    /// }
    ///
    /// fn main() {
    ///     let value = json!({
    ///         "some_value": 42,
    ///     });
    ///
    ///     let package_metadata: SomePackageMetadata = serde_json::from_value(value).unwrap();
    ///     assert_eq!(package_metadata.some_value, 42);
    /// }
    ///
    /// ```
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// The name of a native library the package is linking to.
    pub links: Option<String>,
    /// List of registries to which this package may be published.
    ///
    /// Publishing is unrestricted if `None`, and forbidden if the `Vec` is empty.
    ///
    /// This is always `None` if running with a version of Cargo older than 1.39.
    pub publish: Option<Vec<String>>,
    #[doc(hidden)]
    #[serde(skip)]
    __do_not_match_exhaustively: (),
}

impl Package {
    /// Full path to the license file if one is present in the manifest
    pub fn license_file(&self) -> Option<PathBuf> {
        self.license_file
            .as_ref()
            .map(|file| self.manifest_path.parent().unwrap_or(&self.manifest_path).join(file))
    }

    /// Full path to the readme file if one is present in the manifest
    pub fn readme(&self) -> Option<PathBuf> {
        self.readme
            .as_ref()
            .map(|file| self.manifest_path.join(file))
    }
}

/// The source of a package such as crates.io.
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(transparent)]
pub struct Source {
    /// The underlying string representation of a source.
    pub repr: String,
}

impl Source {
    /// Returns true if the source is crates.io.
    pub fn is_crates_io(&self) -> bool {
        self.repr == "registry+https://github.com/rust-lang/crates.io-index"
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.repr, f)
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
/// A single target (lib, bin, example, ...) provided by a crate
pub struct Target {
    /// Name as given in the `Cargo.toml` or generated from the file name
    pub name: String,
    /// Kind of target ("bin", "example", "test", "bench", "lib")
    pub kind: Vec<String>,
    /// Almost the same as `kind`, except when an example is a library instead of an executable.
    /// In that case `crate_types` contains things like `rlib` and `dylib` while `kind` is `example`
    #[serde(default)]
    pub crate_types: Vec<String>,

    #[serde(default)]
    #[serde(rename = "required-features")]
    /// This target is built only if these features are enabled.
    /// It doesn't apply to `lib` targets.
    pub required_features: Vec<String>,
    /// Path to the main source file of the target
    pub src_path: PathBuf,
    /// Rust edition for this target
    #[serde(default = "edition_default")]
    pub edition: String,
    /// Whether or not this target has doc tests enabled, and the target is
    /// compatible with doc testing.
    ///
    /// This is always `true` if running with a version of Cargo older than 1.37.
    #[serde(default = "default_true")]
    pub doctest: bool,
    #[doc(hidden)]
    #[serde(skip)]
    __do_not_match_exhaustively: (),
}

fn default_true() -> bool {
    true
}

fn edition_default() -> String {
    "2015".to_string()
}

/// Cargo features flags
#[derive(Debug, Clone)]
pub enum CargoOpt {
    /// Run cargo with `--features-all`
    AllFeatures,
    /// Run cargo with `--no-default-features`
    NoDefaultFeatures,
    /// Run cargo with `--features <FEATURES>`
    SomeFeatures(Vec<String>),
}

/// A builder for configurating `cargo metadata` invocation.
#[derive(Debug, Clone, Default)]
pub struct MetadataCommand {
    cargo_path: Option<PathBuf>,
    manifest_path: Option<PathBuf>,
    current_dir: Option<PathBuf>,
    no_deps: bool,
    features: Option<CargoOpt>,
    other_options: Vec<String>,
}

impl MetadataCommand {
    /// Creates a default `cargo metadata` command, which will look for
    /// `Cargo.toml` in the ancestors of the current directory.
    pub fn new() -> MetadataCommand {
        MetadataCommand::default()
    }
    /// Path to `cargo` executable.  If not set, this will use the
    /// the `$CARGO` environment variable, and if that is not set, will
    /// simply be `cargo`.
    pub fn cargo_path(&mut self, path: impl Into<PathBuf>) -> &mut MetadataCommand {
        self.cargo_path = Some(path.into());
        self
    }
    /// Path to `Cargo.toml`
    pub fn manifest_path(&mut self, path: impl Into<PathBuf>) -> &mut MetadataCommand {
        self.manifest_path = Some(path.into());
        self
    }
    /// Current directory of the `cargo metadata` process.
    pub fn current_dir(&mut self, path: impl Into<PathBuf>) -> &mut MetadataCommand {
        self.current_dir = Some(path.into());
        self
    }
    /// Output information only about the root package and don't fetch dependencies.
    pub fn no_deps(&mut self) -> &mut MetadataCommand {
        self.no_deps = true;
        self
    }
    /// Which features to include.
    pub fn features(&mut self, features: CargoOpt) -> &mut MetadataCommand {
        self.features = Some(features);
        self
    }
    /// Arbitrary command line flags to pass to `cargo`.  These will be added
    /// to the end of the command line invocation.
    pub fn other_options(&mut self, options: impl Into<Vec<String>>) -> &mut MetadataCommand {
        self.other_options = options.into();
        self
    }

    /// Builds a command for `cargo metadata`.  This is the first
    /// part of the work of `exec`.
    pub fn cargo_command(&self) -> Result<Command> {
        let cargo = self
            .cargo_path
            .clone()
            .or_else(|| env::var("CARGO").map(|s| PathBuf::from(s)).ok())
            .unwrap_or_else(|| PathBuf::from("cargo"));
        let mut cmd = Command::new(cargo);
        cmd.args(&["metadata", "--format-version", "1"]);

        if self.no_deps {
            cmd.arg("--no-deps");
        }

        if let Some(path) = self.current_dir.as_ref() {
            cmd.current_dir(path);
        }

        if let Some(features) = &self.features {
            match features {
                CargoOpt::AllFeatures => cmd.arg("--all-features"),
                CargoOpt::NoDefaultFeatures => cmd.arg("--no-default-features"),
                CargoOpt::SomeFeatures(ftrs) => cmd.arg("--features").arg(ftrs.join(",")),
            };
        }

        if let Some(manifest_path) = &self.manifest_path {
            cmd.arg("--manifest-path").arg(manifest_path.as_os_str());
        }
        cmd.args(&self.other_options);

        Ok(cmd)
    }

    /// Parses `cargo metadata` output.  `data` must have been
    /// produced by a command built with `cargo_command`.
    pub fn parse<T : AsRef<str>>(data : T) -> Result<Metadata> {
        let meta = serde_json::from_str(data.as_ref())?;
        Ok(meta)
    }

    /// Runs configured `cargo metadata` and returns parsed `Metadata`.
    pub fn exec(&self) -> Result<Metadata> {
        let mut cmd = self.cargo_command()?;
        let output = cmd.output()?;
        if !output.status.success() {
            return Err(Error::CargoMetadata {
                stderr: String::from_utf8(output.stderr)?,
            });
        }
        let stdout = from_utf8(&output.stdout)?
            .lines()
            .find(|line| line.starts_with('{'))
            .ok_or_else(|| Error::NoJson)?;
        Self::parse(stdout)
    }
}