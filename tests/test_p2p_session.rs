mod stubs;

use ggrs::{
    DesyncDetection, GgrsError, GgrsEvent, PlayerHandle, PlayerType, SessionBuilder,
    UdpNonBlockingSocket,
};
use serial_test::serial;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use stubs::{StubConfig, StubInput};

#[test]
#[serial]
fn test_add_more_players() -> Result<(), GgrsError> {
    let socket = UdpNonBlockingSocket::bind_to_port(7777).unwrap();
    let remote_addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
    let remote_addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8081);
    let remote_addr3 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8082);
    let spec_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8090);

    let _sess = SessionBuilder::<StubConfig>::new()
        .add_player(PlayerType::Local, PlayerHandle(0))?
        .add_player(PlayerType::Remote(remote_addr1), PlayerHandle(1))?
        .add_player(PlayerType::Remote(remote_addr2), PlayerHandle(2))?
        .add_player(PlayerType::Remote(remote_addr3), PlayerHandle(3))?
        .add_player(PlayerType::Spectator(spec_addr), PlayerHandle(4))?
        .start_p2p_session(socket)?;
    Ok(())
}

#[test]
#[serial]
fn test_start_session() -> Result<(), GgrsError> {
    let socket = UdpNonBlockingSocket::bind_to_port(7777).unwrap();
    let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
    let spec_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8090);

    let _sess = SessionBuilder::<StubConfig>::new()
        .add_player(PlayerType::Local, PlayerHandle(0))?
        .add_player(PlayerType::Remote(remote_addr), PlayerHandle(1))?
        .add_player(PlayerType::Spectator(spec_addr), PlayerHandle(2))?
        .start_p2p_session(socket)?;
    Ok(())
}

#[test]
#[serial]
fn test_disconnect_player() -> Result<(), GgrsError> {
    let socket = UdpNonBlockingSocket::bind_to_port(7777).unwrap();
    let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
    let spec_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8090);

    let mut sess = SessionBuilder::<StubConfig>::new()
        .add_player(PlayerType::Local, PlayerHandle(0))?
        .add_player(PlayerType::Remote(remote_addr), PlayerHandle(1))?
        .add_player(PlayerType::Spectator(spec_addr), PlayerHandle(2))?
        .start_p2p_session(socket)?;

    assert!(sess.disconnect_player(PlayerHandle(5)).is_err()); // invalid handle
    assert!(sess.disconnect_player(PlayerHandle(0)).is_err()); // for now, local players cannot be disconnected
    assert!(sess.disconnect_player(PlayerHandle(1)).is_ok());
    assert!(sess.disconnect_player(PlayerHandle(1)).is_err()); // already disconnected
    assert!(sess.disconnect_player(PlayerHandle(2)).is_ok());

    Ok(())
}

#[test]
#[serial]
fn test_advance_frame_p2p_sessions() -> Result<(), GgrsError> {
    let addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7777);
    let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8888);

    let socket1 = UdpNonBlockingSocket::bind_to_port(7777).unwrap();
    let mut sess1 = SessionBuilder::<StubConfig>::new()
        .add_player(PlayerType::Local, PlayerHandle(0))?
        .add_player(PlayerType::Remote(addr2), PlayerHandle(1))?
        .start_p2p_session(socket1)?;

    let socket2 = UdpNonBlockingSocket::bind_to_port(8888).unwrap();
    let mut sess2 = SessionBuilder::<StubConfig>::new()
        .add_player(PlayerType::Remote(addr1), PlayerHandle(0))?
        .add_player(PlayerType::Local, PlayerHandle(1))?
        .start_p2p_session(socket2)?;

    for _ in 0..50 {
        sess1.poll_remote_clients();
        sess2.poll_remote_clients();
    }

    let mut stub1 = stubs::GameStub::new();
    let mut stub2 = stubs::GameStub::new();
    let reps = 10;
    for i in 0..reps {
        sess1.poll_remote_clients();
        sess2.poll_remote_clients();

        sess1
            .add_local_input(PlayerHandle(0), StubInput { inp: i })
            .unwrap();
        let requests1 = sess1.advance_frame().unwrap();
        stub1.handle_requests(requests1);
        sess2
            .add_local_input(PlayerHandle(1), StubInput { inp: i })
            .unwrap();
        let requests2 = sess2.advance_frame().unwrap();
        stub2.handle_requests(requests2);

        // gamestate evolves
        assert_eq!(stub1.gs.frame, i as i32 + 1);
        assert_eq!(stub2.gs.frame, i as i32 + 1);
    }

    Ok(())
}

#[test]
#[serial]
fn test_desyncs_detected() -> Result<(), GgrsError> {
    let addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7777);
    let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8888);
    let desync_mode = DesyncDetection::On { interval: 100 };

    let socket1 = UdpNonBlockingSocket::bind_to_port(7777).unwrap();
    let mut sess1 = SessionBuilder::<StubConfig>::new()
        .add_player(PlayerType::Local, PlayerHandle(0))?
        .add_player(PlayerType::Remote(addr2), PlayerHandle(1))?
        .with_desync_detection_mode(desync_mode)
        .start_p2p_session(socket1)?;

    let socket2 = UdpNonBlockingSocket::bind_to_port(8888).unwrap();
    let mut sess2 = SessionBuilder::<StubConfig>::new()
        .add_player(PlayerType::Remote(addr1), PlayerHandle(0))?
        .add_player(PlayerType::Local, PlayerHandle(1))?
        .with_desync_detection_mode(desync_mode)
        .start_p2p_session(socket2)?;

    assert!(sess1.events().chain(sess2.events()).next().is_none());

    let mut stub1 = stubs::GameStub::new();
    let mut stub2 = stubs::GameStub::new();

    // run normally for some frames (past first desync interval)
    for i in 0..110 {
        sess1.poll_remote_clients();
        sess2.poll_remote_clients();

        sess1
            .add_local_input(PlayerHandle(0), StubInput { inp: i })
            .unwrap();
        sess2
            .add_local_input(PlayerHandle(1), StubInput { inp: i })
            .unwrap();

        let requests1 = sess1.advance_frame().unwrap();
        let requests2 = sess2.advance_frame().unwrap();

        stub1.handle_requests(requests1);
        stub2.handle_requests(requests2);
    }

    // check that there are no unexpected events yet
    assert_eq!(sess1.events().len(), 0);
    assert_eq!(sess2.events().len(), 0);

    // run for some more frames
    for _ in 0..100 {
        sess1.poll_remote_clients();
        sess2.poll_remote_clients();

        // mess up state for peer 1
        stub1.gs.state = 1234;

        // keep input steady (to avoid loads, which would restore valid state)
        sess1
            .add_local_input(PlayerHandle(0), StubInput { inp: 0 })
            .unwrap();
        sess2
            .add_local_input(PlayerHandle(1), StubInput { inp: 1 })
            .unwrap();

        let requests1 = sess1.advance_frame().unwrap();
        let requests2 = sess2.advance_frame().unwrap();

        stub1.handle_requests(requests1);
        stub2.handle_requests(requests2);
    }

    // check that we got desync events
    let sess1_events: Vec<_> = sess1.events().collect();
    let sess2_events: Vec<_> = sess2.events().collect();
    assert_eq!(sess1_events.len(), 1);
    assert_eq!(sess2_events.len(), 1);

    let GgrsEvent::DesyncDetected {
        frame: desync_frame1,
        local_checksum: desync_local_checksum1,
        remote_checksum: desync_remote_checksum1,
        addr: desync_addr1,
    } = sess1_events[0]
    else {
        panic!("no desync for peer 1");
    };
    assert_eq!(desync_frame1, 200);
    assert_eq!(desync_addr1, addr2);
    assert_ne!(desync_local_checksum1, desync_remote_checksum1);

    let GgrsEvent::DesyncDetected {
        frame: desync_frame2,
        local_checksum: desync_local_checksum2,
        remote_checksum: desync_remote_checksum2,
        addr: desync_addr2,
    } = sess2_events[0]
    else {
        panic!("no desync for peer 2");
    };
    assert_eq!(desync_frame2, 200);
    assert_eq!(desync_addr2, addr1);
    assert_ne!(desync_local_checksum2, desync_remote_checksum2);

    // check that checksums match
    assert_eq!(desync_remote_checksum1, desync_local_checksum2);
    assert_eq!(desync_remote_checksum2, desync_local_checksum1);

    Ok(())
}

#[test]
#[serial]
fn test_desyncs_and_input_delay_no_panic() -> Result<(), GgrsError> {
    let addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7777);
    let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8888);
    let desync_mode = DesyncDetection::On { interval: 100 };

    let socket1 = UdpNonBlockingSocket::bind_to_port(7777).unwrap();
    let mut sess1 = SessionBuilder::<StubConfig>::new()
        .add_player(PlayerType::Local, PlayerHandle(0))?
        .add_player(PlayerType::Remote(addr2), PlayerHandle(1))?
        .with_input_delay(5)
        .with_desync_detection_mode(desync_mode)
        .start_p2p_session(socket1)?;

    let socket2 = UdpNonBlockingSocket::bind_to_port(8888).unwrap();
    let mut sess2 = SessionBuilder::<StubConfig>::new()
        .add_player(PlayerType::Remote(addr1), PlayerHandle(0))?
        .add_player(PlayerType::Local, PlayerHandle(1))?
        .with_input_delay(5)
        .with_desync_detection_mode(desync_mode)
        .start_p2p_session(socket2)?;

    let mut stub1 = stubs::GameStub::new();
    let mut stub2 = stubs::GameStub::new();

    // run normally for some frames (past first desync interval)
    for i in 0..150 {
        sess1.poll_remote_clients();
        sess2.poll_remote_clients();

        sess1
            .add_local_input(PlayerHandle(0), StubInput { inp: i })
            .unwrap();
        sess2
            .add_local_input(PlayerHandle(1), StubInput { inp: i })
            .unwrap();

        let requests1 = sess1.advance_frame().unwrap();
        let requests2 = sess2.advance_frame().unwrap();

        stub1.handle_requests(requests1);
        stub2.handle_requests(requests2);
    }

    Ok(())
}
