//! SFTP login through an SSH agent.
//!
//! A binary of its own, with one test in it, because the client finds the agent
//! the way every SSH client does — through `SSH_AUTH_SOCK` — and changing the
//! environment is only sound while nothing else in the process might be
//! reading it. Here nothing is: the variable is set before any server or
//! runtime exists.
#![cfg(unix)]

#[path = "support/sftp_server.rs"]
mod support;

use neutron_remote::site::Auth;
use neutron_remote::transport::RemoteError;
use russh::keys::agent::client::AgentClient;
use russh::keys::{Algorithm, PrivateKey};
use support::{FakeSftp, Item, key};

/// Runs russh's own agent on `socket`, holding `keys`.
fn start_agent(socket: &std::path::Path, keys: Vec<PrivateKey>) {
    let listener = std::os::unix::net::UnixListener::bind(socket).expect("bind agent socket");
    listener.set_nonblocking(true).expect("nonblocking");
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            let listener = tokio::net::UnixListener::from_std(listener).expect("from_std");
            let incoming = Box::pin(futures::stream::unfold(listener, |listener| async move {
                let next = listener.accept().await.map(|(stream, _)| stream);
                Some((next, listener))
            }));
            let _ = russh::keys::agent::server::serve(incoming, ()).await;
        });
    });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let mut agent = AgentClient::connect_uds(socket).await.expect("reach agent");
        for key in &keys {
            agent.add_identity(key, &[]).await.expect("add identity");
        }
    });
}

#[test]
fn the_agents_keys_are_offered_until_one_is_accepted() {
    let socket = std::env::temp_dir().join(format!("neutron-agent-{}", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    // SAFETY: the first thing this process's only test does, before any other
    // thread that could read the environment has been started.
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &socket) };

    let wanted = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("key");
    let other = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("key");
    // The wrong key first, so this also shows a refusal of one identity moves
    // on to the next rather than failing the login.
    start_agent(&socket, vec![other, wanted.clone()]);

    // Every password is refused, so only the agent can get this one in.
    let server = FakeSftp::start_accepting(
        key(),
        vec![Item::file("a.txt", b"a")],
        false,
        Some(wanted.public_key().clone()),
    );
    let mut site = server.site(None);
    site.auth = Auth::Agent;

    // Through the public entry point: an agent site needs no saved password,
    // and must not be refused for lacking one.
    let mut transport = neutron_remote::connect(&site).expect("the agent's key should log in");
    assert_eq!(transport.list("/", &mut Vec::new()).expect("list").len(), 1);

    // And a server that knows none of the agent's keys is a login failure.
    let stranger = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("key");
    let server = FakeSftp::start_accepting(key(), Vec::new(), false, Some(stranger.public_key().clone()));
    let mut site = server.site(None);
    site.auth = Auth::Agent;
    let Err(error) = neutron_remote::connect(&site) else {
        panic!("no key in the agent is known to this server");
    };
    assert!(matches!(error, RemoteError::Auth { .. }), "{error:?}");

    let _ = std::fs::remove_file(&socket);
}
