//! `nowcast brick forge configure`, the brick's configuration command, run
//! in the user's terminal by `nowcast bricks enable forge` and `nowcast
//! bricks configure forge`. It asks, forge by forge, where the token comes
//! from: the forge's CLI, a token the user pastes, or nothing. The choices go
//! to the brick's configuration file, a token to the keyring. It never
//! touches the configuration of `gh` or `az`.

use std::io;
use std::process::ExitCode;

use crate::credentials::{Config, Credentials, Forge, Source};

/// What the configuration asks the user.
pub trait Ask {
    /// Where `forge`'s token comes from, `current` offered first; `None` for
    /// a forge not used.
    fn source(&mut self, forge: Forge, current: Option<Source>) -> io::Result<Option<Source>>;
    /// The token for `forge`; `None` keeps the one kept, which only a
    /// forge `keeping` one may do.
    fn token(&mut self, forge: Forge, keeping: bool) -> io::Result<Option<String>>;
    /// Tells the user something that does not stop the configuration.
    fn warn(&mut self, message: &str) -> io::Result<()>;
}

/// Runs the configuration in the terminal, with the user's configuration
/// file and the system keyring.
pub fn run() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("nowcast: cannot configure the forge brick: {e}");
            return ExitCode::FAILURE;
        }
    };
    let credentials = Credentials::system();
    let _ = cliclack::intro("nowcast: the forge brick's tokens");
    match runtime.block_on(configure(&mut Terminal, &credentials)) {
        Ok(()) => {
            let path = credentials.config_path().map(|p| p.display().to_string());
            let _ = cliclack::outro(format!(
                "Saved in {}; a token, in the system keyring",
                path.unwrap_or_default()
            ));
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = cliclack::outro_cancel(e);
            ExitCode::FAILURE
        }
    }
}

/// Asks for each forge's source, and its token when the user gives one,
/// then stores the tokens, writes the configuration and drops the tokens no
/// longer used, in that order: the configuration never names a token the
/// keyring does not hold. Nothing is written when the user cancels.
pub async fn configure(ask: &mut dyn Ask, credentials: &Credentials) -> Result<(), String> {
    let path = credentials
        .config_path()
        .ok_or("no home directory: set HOME or XDG_CONFIG_HOME")?;
    let failed = |e: io::Error| match e.kind() {
        io::ErrorKind::Interrupted => "Cancelled: nothing changed".to_owned(),
        _ => format!("cannot ask: {e}"),
    };
    let mut config = match Config::read(path) {
        Ok(config) => config,
        Err(e) => {
            ask.warn(&format!("{e}: it is written anew"))
                .map_err(failed)?;
            Config::default()
        }
    };
    let mut tokens = Vec::new();
    let mut dropped = Vec::new();
    for forge in Forge::ALL {
        let current = config.source(forge);
        let chosen = ask.source(forge, current).map_err(failed)?;
        let keeping = current == Some(Source::Token);
        if chosen == Some(Source::Token) {
            if let Some(token) = ask.token(forge, keeping).map_err(failed)? {
                tokens.push((forge, token));
            }
        } else if keeping {
            dropped.push(forge);
        }
        config.set(forge, chosen);
    }
    let secrets = credentials.secrets();
    for (forge, token) in &tokens {
        secrets.set(*forge, token).await.map_err(|e| {
            format!(
                "cannot keep the {} token in the system keyring: {e}",
                forge.title()
            )
        })?;
    }
    config
        .write(path)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    // Dropped once the configuration no longer names them. A token no longer
    // used is left behind rather than failing the whole configuration.
    for forge in dropped {
        if let Err(e) = secrets.delete(forge).await {
            let message = format!(
                "the {} token stays in the system keyring: {e}",
                forge.title()
            );
            ask.warn(&message).map_err(failed)?;
        }
    }
    Ok(())
}

/// The user's terminal, through prompts on stderr.
struct Terminal;

impl Ask for Terminal {
    fn source(&mut self, forge: Forge, current: Option<Source>) -> io::Result<Option<Source>> {
        let (cli, login) = match forge {
            Forge::GitHub => ("gh, the GitHub CLI", "its login, `gh auth login`"),
            Forge::Azure => ("az, the Azure CLI", "its login, `az login`"),
        };
        cliclack::select(format!(
            "{}: where does the token come from?",
            forge.title()
        ))
        .item(Some(Source::Cli), cli, login)
        .item(
            Some(Source::Token),
            "A personal access token",
            "pasted here, kept in the system keyring",
        )
        .item(None, "Not used", "its flows publish no_credentials")
        .initial_value(current)
        .interact()
    }

    fn token(&mut self, forge: Forge, keeping: bool) -> io::Result<Option<String>> {
        let mut prompt = cliclack::password(if keeping {
            format!(
                "{} personal access token (empty keeps the one kept)",
                forge.title()
            )
        } else {
            format!("{} personal access token", forge.title())
        })
        .validate(move |token: &String| {
            let token = token.trim();
            if token.is_empty() && !keeping {
                Err("a token is needed")
            } else if token.chars().all(|c| c.is_ascii_graphic()) {
                Ok(())
            } else {
                Err("a token holds no space nor other character")
            }
        });
        if keeping {
            prompt = prompt.allow_empty();
        }
        let token = prompt.interact()?;
        let token = token.trim();
        Ok((!token.is_empty()).then(|| token.to_owned()))
    }

    fn warn(&mut self, message: &str) -> io::Result<()> {
        cliclack::log::warning(message)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::credentials::testing::Fake;

    /// Answers given in advance, and the questions asked.
    #[derive(Default)]
    struct Answers {
        sources: VecDeque<io::Result<Option<Source>>>,
        tokens: VecDeque<Option<String>>,
        asked: Vec<String>,
    }

    impl Ask for Answers {
        fn source(&mut self, forge: Forge, current: Option<Source>) -> io::Result<Option<Source>> {
            self.asked
                .push(format!("{} source, now {current:?}", forge.key()));
            self.sources.pop_front().unwrap()
        }

        fn token(&mut self, forge: Forge, keeping: bool) -> io::Result<Option<String>> {
            self.asked
                .push(format!("{} token, keeping {keeping}", forge.key()));
            Ok(self.tokens.pop_front().unwrap())
        }

        fn warn(&mut self, message: &str) -> io::Result<()> {
            self.asked.push(format!("warn {message}"));
            Ok(())
        }
    }

    fn answers(sources: &[Option<Source>], tokens: &[Option<&str>]) -> Answers {
        Answers {
            sources: sources.iter().map(|s| Ok(*s)).collect(),
            tokens: tokens.iter().map(|t| t.map(str::to_owned)).collect(),
            asked: Vec::new(),
        }
    }

    #[tokio::test]
    async fn each_forge_gets_its_source_and_a_token_goes_to_the_keyring() {
        let fake = Fake::new();
        let mut ask = answers(&[Some(Source::Token), Some(Source::Cli)], &[Some("pat")]);
        configure(&mut ask, &fake.credentials).await.unwrap();
        assert_eq!(
            ask.asked,
            [
                "github source, now None",
                "github token, keeping false",
                "azure source, now None"
            ]
        );
        let creds = &fake.credentials;
        assert_eq!(creds.source(Forge::GitHub), Some(Source::Token));
        assert_eq!(creds.source(Forge::Azure), Some(Source::Cli));
        assert_eq!(fake.secrets.kept(Forge::GitHub).as_deref(), Some("pat"));
        assert_eq!(fake.secrets.kept(Forge::Azure), None);
        let text = std::fs::read_to_string(creds.config_path().unwrap()).unwrap();
        assert!(!text.contains("pat"), "no token in the file: {text}");
    }

    #[tokio::test]
    async fn a_configuration_redone_keeps_or_drops_the_token() {
        let fake = Fake::new();
        fake.choose(Forge::GitHub, Some(Source::Token), Some("old"));
        fake.choose(Forge::Azure, Some(Source::Token), Some("pat"));
        let mut ask = answers(&[Some(Source::Token), None], &[None]);
        configure(&mut ask, &fake.credentials).await.unwrap();
        assert_eq!(ask.asked[1], "github token, keeping true");
        assert_eq!(fake.secrets.kept(Forge::GitHub).as_deref(), Some("old"));
        assert_eq!(fake.secrets.kept(Forge::Azure), None, "not used any more");
        assert_eq!(fake.credentials.source(Forge::Azure), None);

        let mut ask = answers(&[Some(Source::Cli), None], &[]);
        configure(&mut ask, &fake.credentials).await.unwrap();
        assert_eq!(fake.secrets.kept(Forge::GitHub), None);
        assert_eq!(fake.credentials.source(Forge::GitHub), Some(Source::Cli));
    }

    #[tokio::test]
    async fn a_cancelled_configuration_changes_nothing() {
        let fake = Fake::new();
        fake.choose(Forge::GitHub, Some(Source::Cli), None);
        let mut ask = answers(&[Some(Source::Token)], &[Some("pat")]);
        ask.sources
            .push_back(Err(io::Error::from(io::ErrorKind::Interrupted)));
        let error = configure(&mut ask, &fake.credentials).await.unwrap_err();
        assert_eq!(error, "Cancelled: nothing changed");
        assert_eq!(fake.credentials.source(Forge::GitHub), Some(Source::Cli));
        assert_eq!(fake.secrets.kept(Forge::GitHub), None);
    }

    #[tokio::test]
    async fn a_configuration_not_written_keeps_the_token_it_names() {
        use std::os::unix::fs::PermissionsExt;

        let fake = Fake::new();
        fake.choose(Forge::Azure, Some(Source::Token), Some("pat"));
        let dir = fake.credentials.config_path().unwrap().parent().unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let mut ask = answers(&[None, None], &[]);
        let result = configure(&mut ask, &fake.credentials).await;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.unwrap_err().starts_with("cannot write"));
        assert_eq!(fake.credentials.source(Forge::Azure), Some(Source::Token));
        assert_eq!(fake.secrets.kept(Forge::Azure).as_deref(), Some("pat"));
    }

    #[tokio::test]
    async fn an_unreadable_configuration_is_written_anew() {
        let fake = Fake::new();
        let path = fake.credentials.config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "not toml [").unwrap();
        let mut ask = answers(&[None, Some(Source::Cli)], &[]);
        configure(&mut ask, &fake.credentials).await.unwrap();
        assert!(ask.asked[0].starts_with("warn "), "{:?}", ask.asked);
        assert_eq!(fake.credentials.source(Forge::Azure), Some(Source::Cli));
    }
}
