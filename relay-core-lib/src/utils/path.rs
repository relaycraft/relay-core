use std::io;
use std::path::{Path, PathBuf};

/// PathSanitizer ensures that file access is restricted to a specific root directory.
#[derive(Debug, Clone)]
pub struct PathSanitizer {
    root: PathBuf,
}

impl PathSanitizer {
    pub fn new(root: PathBuf) -> Self {
        // We try to canonicalize the root. If it fails (doesn't exist), we keep it as is.
        // Ideally the root should exist.
        let canonical_root = root.canonicalize().unwrap_or(root);
        Self {
            root: canonical_root,
        }
    }

    /// Resolve a relative path against the sandbox root, ensuring it stays within the root.
    /// Returns the absolute canonicalized path if safe.
    pub fn sanitize(&self, path_str: &str) -> io::Result<PathBuf> {
        let path = Path::new(path_str);

        // If path is absolute, we check if it is within root.
        // If relative, we join with root.
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };

        // Canonicalize to resolve .. and symlinks.
        // Note: canonicalize() requires the file to exist.
        // For read operations (BodySource::File, MapLocal), this is expected.
        let canonical = candidate.canonicalize()?;

        if canonical.starts_with(&self.root) {
            Ok(canonical)
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("Path traversal detected: {:?}", canonical),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_sandbox_allows_valid_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "content").unwrap();

        let sanitizer = PathSanitizer::new(temp_dir.path().to_path_buf());
        let result = sanitizer.sanitize("test.txt");

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_sandbox_denies_traversal() {
        let temp_dir = TempDir::new().unwrap();
        let sanitizer = PathSanitizer::new(temp_dir.path().to_path_buf());

        // Try to access /etc/passwd or something outside
        // We use ".." to go up
        let _result = sanitizer.sanitize("../outside.txt");
        // Since outside.txt doesn't exist, canonicalize might fail with NotFound,
        // which is also acceptable as "denied" in a sense, but we want to test boundary.

        // Better: create a file outside
        let outside_dir = TempDir::new().unwrap();
        let outside_file = outside_dir.path().join("outside.txt");
        fs::write(&outside_file, "secret").unwrap();

        // This assumes temp dirs are separate
        let result = sanitizer.sanitize(outside_file.to_str().unwrap());

        assert!(result.is_err());
    }
}
