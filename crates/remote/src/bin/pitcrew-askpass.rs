//! `pitcrew-askpass`: the program ssh runs (via `SSH_ASKPASS`) to ask for a password, a
//! passphrase, a one-time code or a host-key decision. It forwards the question to the PitCrew
//! desktop and prints the answer. See `pitcrew_remote::askpass`.

use pitcrew_remote::askpass::client;

fn main() {
    // Before anything else, while the parent is certainly the ssh that started us.
    let parent = client::Parent::at_start();
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let code = client::main_with(
        &args,
        |name| std::env::var(name).ok(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    if code == client::EXIT_NO_ANSWER {
        // Failing now would make ssh send an empty password and ask again.
        parent.stop_ssh();
    }
    std::process::exit(code);
}
