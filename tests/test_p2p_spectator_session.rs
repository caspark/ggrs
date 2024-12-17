mod stubs;

use ggrs::{GgrsError, PlayerType, SessionBuilder, UdpNonBlockingSocket};
use serial_test::serial;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use stubs::{StubConfig, StubInput};

#[test]
#[serial]
fn test_can_follow_host() -> Result<(), GgrsError> {
    let host_addr: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7777);
    let spec_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8888);

    let socket1 = UdpNonBlockingSocket::bind_to_port(7777).unwrap();
    let mut host_sess = SessionBuilder::<StubConfig>::new()
        .with_num_players(1)
        .add_player(PlayerType::Local, 0)?
        .add_player(PlayerType::Spectator(spec_addr), 2)?
        .start_p2p_session(socket1)?;

    let socket2 = UdpNonBlockingSocket::bind_to_port(8888).unwrap();
    let mut spec_sess =
        SessionBuilder::<StubConfig>::new().start_spectator_session(host_addr, socket2);

    //FIXME figure out why this test is panicking
    let mut host_game = stubs::GameStub::new();
    let mut spec_game = stubs::GameStub::new();

    let reps = 50;
    for i in 0..reps {
        host_sess.poll_remote_clients();
        spec_sess.poll_remote_clients();

        host_sess.add_local_input(0, StubInput { inp: i }).unwrap();
        let requests1 = host_sess.advance_frame().unwrap();
        host_game.handle_requests(requests1);

        if let Ok(requests2) = spec_sess.advance_frame() {
            spec_game.handle_requests(requests2);
        }

        // gamestate evolves
        assert_eq!(host_game.gs.frame, i as i32 + 1);
    }
    //TODO assert that spectator game state has advanced
    Ok(())
}
