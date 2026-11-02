//! Configuration for a preprocessor run: predefined macros and include paths.

use std::path::PathBuf;

/// Default maximum number of nested `` `include `` levels.
pub const DEFAULT_MAX_INCLUDE_DEPTH: usize = 32;

/// Options controlling a preprocessor run.
///
/// This plays the role of the `+define+NAME=VALUE` / `-DNAME=VALUE` and
/// `+incdir+DIR` / `-IDIR` command-line options of other HDL tools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreprocessOptions {
    /// Macros defined before the first line of every file, in order, as
    /// `(name, body)` pairs. `name` may carry a parameter list, e.g.
    /// `("MAX(a,b)", "((a)>(b)?(a):(b))")`. An empty body defines a flag
    /// macro usable with `` `ifdef ``.
    pub defines: Vec<(String, String)>,
    /// Directories searched, in order, for `` `include `` files after the
    /// directory of the including file.
    pub include_dirs: Vec<PathBuf>,
    /// Maximum number of nested `` `include `` levels before an error is
    /// reported (guards against runaway recursion that the cycle check
    /// cannot see, such as includes through symlinks).
    pub max_include_depth: usize,
}

impl Default for PreprocessOptions {
    fn default() -> Self {
        Self {
            defines: Vec::new(),
            include_dirs: Vec::new(),
            max_include_depth: DEFAULT_MAX_INCLUDE_DEPTH,
        }
    }
}

impl PreprocessOptions {
    /// Returns a copy of these options with one more predefined macro.
    pub fn with_define(mut self, name: impl Into<String>, body: impl Into<String>) -> Self {
        self.defines.push((name.into(), body.into()));
        self
    }

    /// Returns a copy of these options with one more include search directory.
    pub fn with_include_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.include_dirs.push(dir.into());
        self
    }

    /// Adds a predefined macro from command-line syntax `NAME` or `NAME=VALUE`
    /// (a bare `NAME` defines it with an empty body, like `+define+NAME`).
    ///
    /// Returns `false`, leaving the options unchanged, when the name part is empty.
    pub fn add_define_arg(&mut self, arg: &str) -> bool {
        let (name, value) = match arg.split_once('=') {
            Some((n, v)) => (n.trim(), v),
            None => (arg.trim(), ""),
        };
        if name.is_empty() {
            return false;
        }
        self.defines.push((name.to_string(), value.to_string()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_no_defines_and_standard_depth() {
        let o = PreprocessOptions::default();
        assert!(o.defines.is_empty());
        assert!(o.include_dirs.is_empty());
        assert_eq!(o.max_include_depth, DEFAULT_MAX_INCLUDE_DEPTH);
    }

    #[test]
    fn builders_append_in_order() {
        let o = PreprocessOptions::default()
            .with_define("A", "1")
            .with_define("B", "")
            .with_include_dir("inc");
        assert_eq!(
            o.defines,
            vec![("A".into(), "1".into()), ("B".into(), String::new())]
        );
        assert_eq!(o.include_dirs, vec![PathBuf::from("inc")]);
    }

    #[test]
    fn add_define_arg_parses_name_and_value() {
        let mut o = PreprocessOptions::default();
        assert!(o.add_define_arg("WIDTH=8"));
        assert!(o.add_define_arg("SYNTHESIS"));
        assert!(o.add_define_arg("EQ=a=b"));
        assert_eq!(
            o.defines,
            vec![
                ("WIDTH".into(), "8".into()),
                ("SYNTHESIS".into(), String::new()),
                ("EQ".into(), "a=b".into()),
            ]
        );
    }

    #[test]
    fn add_define_arg_rejects_empty_name() {
        let mut o = PreprocessOptions::default();
        assert!(!o.add_define_arg(""));
        assert!(!o.add_define_arg("=5"));
        assert!(o.defines.is_empty());
    }
}
