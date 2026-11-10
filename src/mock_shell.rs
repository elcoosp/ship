use crate::shell::{Output, Shell};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A test double for `Shell` that records all invocations and returns
/// predefined outputs based on the first argument (e.g., "git fetch").
#[derive(Clone, Default)]
pub struct MockShell {
    pub calls: Arc<Mutex<Vec<Vec<String>>>>,
    pub responses: Arc<Mutex<HashMap<String, Output>>>,
    pub cwd: PathBuf,
}

impl MockShell {
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(HashMap::new())),
            cwd: PathBuf::from("/mock"),
        }
    }

    pub fn with_cwd(cwd: &str) -> Self {
        let mut s = Self::new();
        s.cwd = PathBuf::from(cwd);
        s
    }

    /// Set the response for a specific command prefix (joined by spaces).
    /// e.g., `mock.stub("git status", 0, "", "")`
    pub fn stub(&self, cmd_prefix: &str, status: i32, stdout: &str, stderr: &str) {
        let out = Output {
            status,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        };
        self.responses
            .lock()
            .unwrap()
            .insert(cmd_prefix.to_string(), out);
    }

    /// Assert that a command was called.
    pub fn assert_called(&self, expected: &str) {
        let calls = self.calls.lock().unwrap();
        let found = calls.iter().any(|args| args.join(" ").contains(expected));
        assert!(
            found,
            "Expected command containing '{expected}' was not called.\nCalls: {:#?}",
            *calls
        );
    }

    pub fn assert_not_called(&self, expected: &str) {
        let calls = self.calls.lock().unwrap();
        let found = calls.iter().any(|args| args.join(" ").contains(expected));
        assert!(
            !found,
            "Expected command containing '{expected}' WAS called.\nCalls: {:#?}",
            *calls
        );
    }
}

impl Shell for MockShell {
    fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn run(&self, argv: &[&str]) -> Result<Output> {
        if argv.is_empty() {
            anyhow::bail!("empty argv");
        }
        let cmd_str = argv.join(" ");
        self.calls
            .lock()
            .unwrap()
            .push(argv.iter().map(|s| s.to_string()).collect());

        let responses = self.responses.lock().unwrap();

        // Find the longest matching prefix
        let mut best_match: Option<&Output> = None;
        let mut best_len = 0;

        for (prefix, out) in responses.iter() {
            if cmd_str.starts_with(prefix) && prefix.len() > best_len {
                best_match = Some(out);
                best_len = prefix.len();
            }
        }

        if let Some(out) = best_match {
            Ok(out.clone())
        } else {
            // Default fallback for commands we don't explicitly stub
            Ok(Output {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }
}
