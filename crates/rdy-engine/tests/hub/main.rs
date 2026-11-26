//! Hub tests: config, credentials, keys, remotes, transport and publish, grouped into one test
//! binary (one module per former root file).

// Helpers the module files share; each file uses part of them.
#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;
#[path = "../support/hub_tls.rs"]
mod hub_tls;
#[allow(dead_code)]
#[path = "../modes/mod.rs"]
mod modes;

mod get_encoding;
mod hub_config;
mod hub_credentials;
mod hub_http;
mod hub_keys;
mod hub_publish;
mod hub_publish_files;
mod hub_publish_stream;
mod hub_remotes;
mod hub_types;
mod hub_upload_http;
