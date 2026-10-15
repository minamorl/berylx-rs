//! Shared fixtures for the integration tests.
//!
//! Each integration test is its own crate, so items only some of them use look
//! dead to the others.
#![allow(dead_code)]

use berylx::{Conflict, Merge};

/// A state that remembers the order things happened to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trail(pub Vec<String>);

impl Trail {
    pub fn push(&mut self, name: &str) {
        self.0.push(name.to_owned());
    }

    pub fn as_slice(&self) -> Vec<&str> {
        self.0.iter().map(String::as_str).collect()
    }
}

impl Merge for Trail {
    fn merge(_base: &Self, mut left: Self, right: Self) -> Result<Self, Conflict> {
        left.0.extend(right.0);
        Ok(left)
    }
}
