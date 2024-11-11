use crate::{
    path::{StorePath, StorePathSegment},
    ArcStore, KeyMap, Store, StoreFieldTrigger,
};
use or_poisoned::OrPoisoned;
use reactive_graph::{
    owner::Storage,
    signal::{
        guards::{Plain, UntrackedWriteGuard, WriteGuard},
        ArcTrigger,
    },
    traits::{DefinedAt, Track, UntrackableGuard},
    unwrap_signal,
};
use std::{iter, ops::Deref, sync::Arc};

pub trait StoreField: Sized {
    type Value;
    type Reader: Deref<Target = Self::Value>;
    type Writer: UntrackableGuard<Target = Self::Value>;

    fn get_trigger(&self, path: StorePath) -> StoreFieldTrigger;

    fn path(&self) -> impl IntoIterator<Item = StorePathSegment>;

    fn track_field(&self) {
        let path = self.path().into_iter().collect();
        let trigger = self.get_trigger(path);
        trigger.this.track();
        trigger.children.track();
    }

    fn reader(&self) -> Option<Self::Reader>;

    fn writer(&self) -> Option<Self::Writer>;

    fn keys(&self) -> Option<KeyMap>;
}

impl<T> StoreField for ArcStore<T>
where
    T: 'static,
{
    type Value = T;
    type Reader = Plain<T>;
    type Writer = WriteGuard<ArcTrigger, UntrackedWriteGuard<T>>;

    fn get_trigger(&self, path: StorePath) -> StoreFieldTrigger {
        let triggers = &self.signals;
        let trigger = triggers.write().or_poisoned().get_or_insert(path);
        trigger
    }

    fn path(&self) -> impl IntoIterator<Item = StorePathSegment> {
        iter::empty()
    }

    fn reader(&self) -> Option<Self::Reader> {
        Plain::try_new(Arc::clone(&self.value))
    }

    fn writer(&self) -> Option<Self::Writer> {
        let trigger = self.get_trigger(Default::default());
        let guard = UntrackedWriteGuard::try_new(Arc::clone(&self.value))?;
        Some(WriteGuard::new(trigger.children, guard))
    }

    fn keys(&self) -> Option<KeyMap> {
        Some(self.keys.clone())
    }
}

impl<T, S> StoreField for Store<T, S>
where
    T: 'static,
    S: Storage<ArcStore<T>>,
{
    type Value = T;
    type Reader = Plain<T>;
    type Writer = WriteGuard<ArcTrigger, UntrackedWriteGuard<T>>;

    fn get_trigger(&self, path: StorePath) -> StoreFieldTrigger {
        self.inner
            .try_get_value()
            .map(|n| n.get_trigger(path))
            .unwrap_or_else(unwrap_signal!(self))
    }

    fn path(&self) -> impl IntoIterator<Item = StorePathSegment> {
        self.inner
            .try_get_value()
            .map(|n| n.path().into_iter().collect::<Vec<_>>())
            .unwrap_or_else(unwrap_signal!(self))
    }

    fn reader(&self) -> Option<Self::Reader> {
        self.inner.try_get_value().and_then(|n| n.reader())
    }

    fn writer(&self) -> Option<Self::Writer> {
        self.inner.try_get_value().and_then(|n| n.writer())
    }

    fn keys(&self) -> Option<KeyMap> {
        self.inner.try_get_value().and_then(|inner| inner.keys())
    }
}