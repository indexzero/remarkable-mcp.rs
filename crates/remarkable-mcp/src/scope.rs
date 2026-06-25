//! Optional root-path scoping (`REMARKABLE_ROOT_PATH`), from SamMorrowDrums.
//!
//! When a root is configured, the user sees and supplies paths *relative to* that
//! root, while the library underneath uses absolute device paths. These two pure
//! functions are the whole contract, and are unit-tested.

/// A path scope: either the whole library (`None`) or a subfolder root.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    root: Option<String>,
}

impl Scope {
    /// Build a scope from an optional root path. A root of `/` or empty is treated
    /// as "no scope".
    pub fn new(root: Option<String>) -> Self {
        let root = root.and_then(|r| {
            let r = r.trim().trim_end_matches('/').to_string();
            if r.is_empty() || r == "/" {
                None
            } else if r.starts_with('/') {
                Some(r)
            } else {
                Some(format!("/{r}"))
            }
        });
        Scope { root }
    }

    /// `true` if no scoping is in effect.
    pub fn is_unscoped(&self) -> bool {
        self.root.is_none()
    }

    /// The configured root, if any.
    pub fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }

    /// Resolve a user-supplied (relative) path to an absolute device path.
    pub fn resolve(&self, user_path: &str) -> String {
        let p = user_path.trim();
        let p = if p.is_empty() { "/" } else { p };
        let p = if p.starts_with('/') {
            p.to_string()
        } else {
            format!("/{p}")
        };
        match &self.root {
            None => p,
            Some(root) => {
                if p == "/" {
                    root.clone()
                } else {
                    format!("{root}{p}")
                }
            }
        }
    }

    /// Convert an absolute device path back to what the user should see.
    pub fn display(&self, device_path: &str) -> String {
        match &self.root {
            None => device_path.to_string(),
            Some(root) => {
                if device_path == root {
                    "/".to_string()
                } else if let Some(rest) = device_path.strip_prefix(&format!("{root}/")) {
                    format!("/{rest}")
                } else {
                    device_path.to_string()
                }
            }
        }
    }

    /// `true` if a device path is inside the scope (always true when unscoped).
    pub fn contains(&self, device_path: &str) -> bool {
        match &self.root {
            None => true,
            Some(root) => device_path == root || device_path.starts_with(&format!("{root}/")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unscoped_is_identity() {
        let s = Scope::new(None);
        assert!(s.is_unscoped());
        assert_eq!(s.resolve("/Work/Notes"), "/Work/Notes");
        assert_eq!(s.resolve("Work"), "/Work");
        assert_eq!(s.display("/Work/Notes"), "/Work/Notes");
        assert!(s.contains("/anything"));
    }

    #[test]
    fn root_normalizes() {
        assert!(Scope::new(Some("/".into())).is_unscoped());
        assert!(Scope::new(Some("".into())).is_unscoped());
        assert_eq!(Scope::new(Some("Work".into())).root(), Some("/Work"));
        assert_eq!(Scope::new(Some("/Work/".into())).root(), Some("/Work"));
    }

    #[test]
    fn scoped_resolve_and_display_roundtrip() {
        let s = Scope::new(Some("/Work".into()));
        assert_eq!(s.resolve("/"), "/Work");
        assert_eq!(s.resolve("/Notes"), "/Work/Notes");
        assert_eq!(s.resolve("Notes"), "/Work/Notes");
        assert_eq!(s.display("/Work"), "/");
        assert_eq!(s.display("/Work/Notes"), "/Notes");
        // Outside the scope: left untouched.
        assert_eq!(s.display("/Personal/x"), "/Personal/x");
    }

    #[test]
    fn contains_respects_boundary() {
        let s = Scope::new(Some("/Work".into()));
        assert!(s.contains("/Work"));
        assert!(s.contains("/Work/Notes"));
        assert!(!s.contains("/Workshop")); // prefix but not a path boundary
        assert!(!s.contains("/Personal"));
    }
}
