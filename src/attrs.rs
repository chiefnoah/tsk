use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::collections::btree_map::{IntoIter as BTreeIntoIter, Iter as BTreeMapIter};
use std::iter::Chain;

type Map = BTreeMap<String, String>;

#[allow(dead_code)]
/// Holds xattributes in a way that allows for differentiating between attributes that have been
/// added/modified or that were present when reading the file. This is an *optimization* over
/// infrequently modified values.
#[derive(Default, Clone, Debug)]
pub(crate) struct Attrs {
    pub written: Map,
    pub updated: Map,
}

impl IntoIterator for Attrs {
    type Item = (String, String);

    type IntoIter = Chain<BTreeIntoIter<String, String>, BTreeIntoIter<String, String>>;

    fn into_iter(self) -> Self::IntoIter {
        self.written.into_iter().chain(self.updated)
    }
}

#[allow(dead_code)]
impl Attrs {
    pub(crate) fn from_written(written: Map) -> Self {
        Self {
            written,
            ..Default::default()
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&String> {
        self.updated.get(key).or_else(|| self.written.get(key))
    }

    pub(crate) fn insert(&mut self, key: String, value: String) -> Option<String> {
        match self.updated.entry(key.clone()) {
            Entry::Occupied(mut e) => Some(e.insert(value)),
            Entry::Vacant(e) => {
                e.insert(value);
                let maybe_old_value = self.written.get(&key);
                maybe_old_value.cloned()
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.updated.is_empty() && self.written.is_empty()
    }

    pub(crate) fn iter(
        &self,
    ) -> Chain<BTreeMapIter<'_, String, String>, BTreeMapIter<'_, String, String>> {
        self.written.iter().chain(self.updated.iter())
    }
}
