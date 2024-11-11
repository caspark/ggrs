
p2:
    cargo run --example ex_game_p2p -- --local-port 7000 --players localhost 127.0.0.1:7001 &
    cargo run --example ex_game_p2p -- --local-port 7001 --players 127.0.0.1:7000 localhost &

p3:
    cargo run --example ex_game_p2p -- --local-port 7000 --players localhost 127.0.0.1:7001 127.0.0.1:7002 &
    cargo run --example ex_game_p2p -- --local-port 7001 --players 127.0.0.1:7000 localhost 127.0.0.1:7002 &
    cargo run --example ex_game_p2p -- --local-port 7002 --players 127.0.0.1:7000 127.0.0.1:7001 localhost &
