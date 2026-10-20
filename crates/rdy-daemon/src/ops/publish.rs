//! The publish helper (D-Q0AVZ, D-PJ65F): one tag's publication to a publish remote's hub site,
//! as a candidate. The daemon's `Engine::publish` (`rdy_publish`) runs it, and so does the CLI's
//! publish op; it runs the coordinator ([`PublishService`]) with this crate's renderer, off the
//! writer, from one read state. The user config, and so the credentials, are this process's own.
//!
//! Both HTTPS senders are built on their first request and fenced to that request's install,
//! the selected remote's (the coordinator checks the same fence first), and dropped with the
//! call. Nothing of the call is kept.
use rdy_engine::config_levels::user_file_from_env;
use rdy_engine::hub_config::HubConfig;
use rdy_engine::hub_credentials::require_user_file;
use rdy_engine::hub_http::HttpHubTransport;
use rdy_engine::hub_publish::{PublishRequest, PublishService, result_json};
use rdy_engine::hub_publish_wire::{
    Candidate, Discovery, FileIdentity, HttpPublishWire, ObjectPut, PublicationInputs,
    PublishState, PublishWire, StateRequest, UploadContext,
};
use rdy_engine::hub_types::{HubCall, HubFailure, HubReply, HubTransport, Origin, SystemHubClock};
use rdy_engine::publication_manifest::Manifest;
use rdy_engine::{RdyError, ReadState, Session};
use rdy_model::Hash;
use rdy_render::publish::PublicationRenderer;
use rdy_store::object_refs::ObjectKey;
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// The HTTPS transport fenced to `install`, from the settings of `project` and `user_file`.
fn fenced(
    project: &Path,
    user_file: &Path,
    install: &Origin,
) -> Result<HttpHubTransport, RdyError> {
    let settings = HubConfig::load(Some(project), Some(user_file))?;
    HttpHubTransport::new(settings.fence(install)?)
}

/// The credential transport of one publish call, built on its first request.
struct CredentialTransport<'a> {
    project: &'a Path,
    user_file: &'a Path,
    built: OnceLock<Result<HttpHubTransport, RdyError>>,
}

impl HubTransport for CredentialTransport<'_> {
    fn exchange(&self, call: HubCall) -> Result<HubReply, HubFailure> {
        let built = self
            .built
            .get_or_init(|| fenced(self.project, self.user_file, &call.install));
        match built {
            Ok(t) => t.exchange(call),
            Err(e) => Err(HubFailure::Local(e.clone())),
        }
    }
}

/// The upload wire of one publish call, built on its first request.
struct UploadWire<'a> {
    project: &'a Path,
    user_file: &'a Path,
    built: OnceLock<Result<HttpPublishWire, RdyError>>,
}

impl UploadWire<'_> {
    fn wire(&self, cx: &UploadContext) -> Result<&HttpPublishWire, RdyError> {
        let built = self.built.get_or_init(|| {
            fenced(self.project, self.user_file, &cx.install).map(HttpPublishWire::new)
        });
        built.as_ref().map_err(Clone::clone)
    }
}

impl PublishWire for UploadWire<'_> {
    fn discovery(&self, cx: &UploadContext, remote: &str) -> Result<Discovery, RdyError> {
        self.wire(cx)?.discovery(cx, remote)
    }

    fn state(&self, cx: &UploadContext, request: &StateRequest) -> Result<PublishState, RdyError> {
        self.wire(cx)?.state(cx, request)
    }

    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        self.wire(cx)?.known(cx, push, keys)
    }

    fn put_objects(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError> {
        self.wire(cx)?.put_objects(cx, push, body, len)
    }

    fn known_files(
        &self,
        cx: &UploadContext,
        push: &str,
        files: &[FileIdentity],
    ) -> Result<Vec<Hash>, RdyError> {
        self.wire(cx)?.known_files(cx, push, files)
    }

    fn put_file(
        &self,
        cx: &UploadContext,
        push: &str,
        file: &FileIdentity,
        media_type: &str,
        body: &mut dyn Read,
    ) -> Result<FileIdentity, RdyError> {
        self.wire(cx)?.put_file(cx, push, file, media_type, body)
    }

    fn publish(
        &self,
        cx: &UploadContext,
        push: &str,
        site: &str,
        record: &PublicationInputs,
        manifest: &Manifest,
        keep_latest: Option<bool>,
    ) -> Result<Candidate, RdyError> {
        self.wire(cx)?
            .publish(cx, push, site, record, manifest, keep_latest)
    }
}

/// Publishes `req` from the read state `st` of the project at `root` for `session`, and replies
/// with the publish result (`{id, state, url, push, audience, record, references, inventory,
/// warnings, render_warnings, review}`), serialized through its bounded sink.
pub(crate) fn run(
    st: Arc<ReadState>,
    root: &Path,
    session: &Session,
    req: PublishRequest,
) -> Result<Value, RdyError> {
    let user_file = require_user_file(user_file_from_env())?;
    let api = CredentialTransport {
        project: root,
        user_file: &user_file,
        built: OnceLock::new(),
    };
    let wire = UploadWire {
        project: root,
        user_file: &user_file,
        built: OnceLock::new(),
    };
    let clock = SystemHubClock;
    let result = PublishService::new(root, &user_file, &api, &clock, &wire).run(
        st,
        session,
        req,
        &PublicationRenderer,
    )?;
    result_json(&result)
}
