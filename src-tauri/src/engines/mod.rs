//! Engine modules (ADR 0001): one self-contained adapter per engine, with its own connection options, typed values and
//! statement classification. They are wired into the core ports in the engine-module refactor; until then nothing outside
//! the live tests calls them.
#![allow(dead_code)]

pub mod elasticsearch;
pub mod redis;
