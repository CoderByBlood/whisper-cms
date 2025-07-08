use inquire::error::InquireResult;
use inquire::{CustomType, Password, Text};

#[derive(Debug, PartialEq)]
pub enum Commands {
    Install {
        password: String,
        output: String,
        pghost: String,
        pgport: u16,
        pguser: String,
        pgpassword: String,
        pgdatabase: String,
    },
    Start {
        password: String,
        input: String,
    },
    Rotate {
        old: String,
        new: String,
        config: String,
    },
}

pub fn prompt_for_install(
    password: &Option<String>,
    output: &Option<String>,
    pghost: &Option<String>,
    pgport: &Option<u16>,
    pguser: &Option<String>,
    pgpassword: &Option<String>,
    pgdatabase: &Option<String>,
) -> InquireResult<Commands> {
    println!("Please answer the following questions:");

    let password = match password {
        Some(x) => x.to_string(),
        None => Password::new("Enter password to encrypt the config file").prompt()?,
    };

    let output = match output {
        Some(x) => x.to_string(),
        None => Text::new("Enter output encrypted config file path").prompt()?,
    };

    let pghost = match pghost {
        Some(x) => x.to_string(),
        None => Text::new("PostgreSQL host").prompt()?,
    };

    let pgport = match pgport {
        Some(x) => *x,
        None => CustomType::<u16>::new("PostgreSQL port").prompt()?,
    };

    let pguser = match pguser {
        Some(x) => x.to_string(),
        None => Text::new("PostgreSQL username").prompt()?,
    };

    let pgpassword = match pgpassword {
        Some(x) => x.to_string(),
        None => Password::new("PostgreSQL password").prompt()?,
    };

    let pgdatabase = match pgdatabase {
        Some(x) => x.to_string(),
        None => Text::new("PostgreSQL database name").prompt()?,
    };

    Ok(Commands::Install {
        password,
        output,
        pghost,
        pgport,
        pguser,
        pgpassword,
        pgdatabase,
    })
}

pub fn prompt_for_start(
    password: &Option<String>,
    input: &Option<String>,
) -> InquireResult<Commands> {
    println!("Please answer the following questions:");

    let password = match password {
        Some(x) => x.to_string(),
        None => Password::new("Enter password to decrypt the config file").prompt()?,
    };

    let input = match input {
        Some(x) => x.to_string(),
        None => Text::new("Enter input encrypted config file path").prompt()?,
    };

    Ok(Commands::Start { password, input })
}

pub fn prompt_for_rotate(
    old: &Option<String>,
    new: &Option<String>,
    config: &Option<String>,
) -> InquireResult<Commands> {
    println!("Please answer the following questions:");

    let old = match old {
        Some(x) => x.to_string(),
        None => Password::new("Old password to decrypt the config").prompt()?,
    };

    let new = match new {
        Some(x) => x.to_string(),
        None => Password::new("New password to encrypt the config").prompt()?,
    };

    let config = match config {
        Some(x) => x.to_string(),
        None => Text::new("Encrypted config file path").prompt()?,
    };

    Ok(Commands::Rotate { old, new, config })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commands_enum_construction() {
        let install = Commands::Install {
            password: "pw".into(),
            output: "config.enc".into(),
            pghost: "localhost".into(),
            pgport: 5432,
            pguser: "admin".into(),
            pgpassword: "dbpw".into(),
            pgdatabase: "mydb".into(),
        };

        if let Commands::Install {
            password,
            output,
            pghost,
            pgport,
            pguser,
            pgpassword,
            pgdatabase,
        } = install
        {
            assert_eq!(password, "pw");
            assert_eq!(output, "config.enc");
            assert_eq!(pghost, "localhost");
            assert_eq!(pgport, 5432);
            assert_eq!(pguser, "admin");
            assert_eq!(pgpassword, "dbpw");
            assert_eq!(pgdatabase, "mydb");
        } else {
            panic!("Expected Install variant");
        }

        let start = Commands::Start {
            password: "startpw".into(),
            input: "in.enc".into(),
        };
        if let Commands::Start { password, input } = start {
            assert_eq!(password, "startpw");
            assert_eq!(input, "in.enc");
        } else {
            panic!("Expected Start variant");
        }

        let rotate = Commands::Rotate {
            old: "oldpw".into(),
            new: "newpw".into(),
            config: "file.enc".into(),
        };
        if let Commands::Rotate { old, new, config } = rotate {
            assert_eq!(old, "oldpw");
            assert_eq!(new, "newpw");
            assert_eq!(config, "file.enc");
        } else {
            panic!("Expected Rotate variant");
        }
    }

    #[test]
    fn test_prompt_for_install_all_args_provided() {
        let result = prompt_for_install(
            &Some("pw".into()),
            &Some("out.enc".into()),
            &Some("localhost".into()),
            &Some(5432),
            &Some("admin".into()),
            &Some("dbpw".into()),
            &Some("mydb".into()),
        )
        .expect("Should not error");

        if let Commands::Install {
            password,
            output,
            pghost,
            pgport,
            pguser,
            pgpassword,
            pgdatabase,
        } = result
        {
            assert_eq!(password, "pw");
            assert_eq!(output, "out.enc");
            assert_eq!(pghost, "localhost");
            assert_eq!(pgport, 5432);
            assert_eq!(pguser, "admin");
            assert_eq!(pgpassword, "dbpw");
            assert_eq!(pgdatabase, "mydb");
        } else {
            panic!("Expected Install variant");
        }
    }

    #[test]
    fn test_prompt_for_start_all_args_provided() {
        let result = prompt_for_start(&Some("startpw".into()), &Some("in.enc".into()))
            .expect("Should not error");

        match result {
            Commands::Start { password, input } => {
                assert_eq!(password, "startpw");
                assert_eq!(input, "in.enc");
            }
            _ => panic!("Expected Start command"),
        }
    }

    #[test]
    fn test_prompt_for_rotate_all_args_provided() {
        let result = prompt_for_rotate(
            &Some("oldpw".into()),
            &Some("newpw".into()),
            &Some("file.enc".into()),
        )
        .expect("Should not error");

        match result {
            Commands::Rotate { old, new, config } => {
                assert_eq!(old, "oldpw");
                assert_eq!(new, "newpw");
                assert_eq!(config, "file.enc");
            }
            _ => panic!("Expected Rotate command"),
        }
    }

    /// "Negative" tests
    ///
    /// These will fail at runtime if any parameter is None, because inquire will try to prompt.
    /// In unit test context (non-interactive), this causes an error or hang.
    ///
    /// To avoid that, we can only *confirm* that these calls would *need* user input if None.
    ///
    /// So here we test that calling with None will error immediately in test context.
    /// You may skip these or leave them ignored by default.

    #[test]
    #[should_panic(expected = "prompt")]
    #[ignore]
    fn test_prompt_for_install_with_missing_args_panics() {
        let _ = prompt_for_install(&None, &None, &None, &None, &None, &None, &None).unwrap();
    }

    #[test]
    #[should_panic(expected = "prompt")]
    #[ignore]
    fn test_prompt_for_start_with_missing_args_panics() {
        let _ = prompt_for_start(&None, &None).unwrap();
    }

    #[test]
    #[should_panic(expected = "prompt")]
    #[ignore]
    fn test_prompt_for_rotate_with_missing_args_panics() {
        let _ = prompt_for_rotate(&None, &None, &None).unwrap();
    }
}
