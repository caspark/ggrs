mod ex_game;

use std::collections::BTreeSet;

use ex_game::Game;
use ggrs::{PlayerHandle, SessionBuilder};
use instant::{Duration, Instant};
use macroquad::prelude::*;
use structopt::StructOpt;

const FPS: f64 = 60.0;

/// returns a window config for macroquad to use
fn window_conf() -> Conf {
    Conf {
        window_title: "Box Game Synctest".to_owned(),
        window_width: 600,
        window_height: 800,
        window_resizable: false,
        high_dpi: true,
        ..Default::default()
    }
}

#[derive(StructOpt)]
struct Opt {
    #[structopt(short, long)]
    num_players: usize,
    #[structopt(short, long)]
    check_distance: usize,
}

#[macroquad::main(window_conf)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // configure logging: output ggrs and example game logs to standard out
    tracing::subscriber::set_global_default(
        tracing_subscriber::FmtSubscriber::builder()
            .with_max_level(tracing::Level::DEBUG)
            .finish(),
    )
    .expect("setting up tracing subscriber failed");
    // forward logs from log crate to the tracing subscriber (allows seeing macroquad logs)
    tracing_log::LogTracer::init()?;

    // read cmd line arguments
    let opt = Opt::from_args();

    // create a GGRS session
    let mut sess = SessionBuilder::new()
        .with_check_distance(opt.check_distance)
        .with_input_delay(2)
        .start_synctest_session()?;

    // Create a new box game
    let all_players: BTreeSet<PlayerHandle> = {
        let mut all_players = BTreeSet::new();
        for player_number in 0..opt.num_players {
            all_players.insert(player_number.try_into()?);
        }
        all_players
    };
    let mut game = Game::new(all_players.clone());
    game.register_local_handles(all_players.clone());

    // time variables for tick rate
    let mut last_update = Instant::now();
    let mut accumulator = Duration::ZERO;
    let fps_delta = 1. / FPS;

    loop {
        // get delta time from last iteration and accumulate it
        let delta = Instant::now().duration_since(last_update);
        accumulator = accumulator.saturating_add(delta);
        last_update = Instant::now();

        // if enough time is accumulated, we run a frame
        while accumulator.as_secs_f64() > fps_delta {
            // decrease accumulator
            accumulator = accumulator.saturating_sub(Duration::from_secs_f64(fps_delta));

            // gather inputs
            for handle in sess.local_player_handles() {
                sess.add_local_input(handle, game.local_input(handle))?;
            }

            match sess.advance_frame() {
                Ok(requests) => game.handle_requests(requests, false),
                Err(e) => return Err(Box::new(e)),
            }
        }

        // render the game state
        game.render();

        // wait for the next loop (macroquads wants it so)
        next_frame().await;
    }
}
