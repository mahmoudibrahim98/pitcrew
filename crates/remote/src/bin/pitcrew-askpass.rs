//! `pitcrew-askpass`: the program ssh runs (via `SSH_ASKPASS`) to ask for a password, a
//! passphrase, a one-time code or a host-key decision. It forwards the question to the PitCrew
//! desktop and prints the answer. See `pitcrew_remote::askpass`.

fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let code = pitcrew_remote::askpass::client::main_with(
        &args,
        |name| std::env::var(name).ok(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    std::process::exit(code);
}
