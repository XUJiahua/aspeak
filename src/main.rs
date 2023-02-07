mod cli;

use cli::{commands::Command, Cli};

use aspeak::{
    interpolate_ssml, AspeakError, AudioFormat, SynthesizerConfig, Voice, ORIGIN, QUALITY_MAP,
};
use clap::Parser;
use color_eyre::Help;
use colored::Colorize;

use log::debug;

use reqwest::header::{self, HeaderMap, HeaderValue};
use strum::IntoEnumIterator;
use tokio_tungstenite::tungstenite::{error::ProtocolError, Error as TungsteniteError};

use crate::cli::config::Config;

fn main() -> color_eyre::eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    env_logger::builder().filter_level(cli.log_level()).init();
    debug!("Commandline args: {cli:?}");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    rt.block_on(async {
        match cli.command.unwrap_or_default() {
            Command::SSML {
                ssml,
                input_args,
                output_args,
            } => {
                let ssml = ssml
                    .ok_or(AspeakError::InputError)
                    .or_else(|_| Cli::process_input(input_args))?;
                let (callback, format) = Cli::process_output(output_args)?;
                let synthesizer = SynthesizerConfig::new((&cli.auth).try_into()?, format)
                    .connect()
                    .await?;
                synthesizer.synthesize(&ssml, callback).await?
            }
            Command::Text {
                mut text_args,
                input_args,
                output_args,
            } => {
                text_args.text = Some(
                    text_args
                        .text
                        .ok_or(AspeakError::InputError)
                        .or_else(|_| Cli::process_input(input_args))?,
                );
                let (callback, format) = Cli::process_output(output_args)?;
                let synthesizer = SynthesizerConfig::new((&cli.auth).try_into()?, format)
                    .connect()
                    .await?;
                let ssml = interpolate_ssml((&text_args).try_into()?)?;
                let result = synthesizer.synthesize(&ssml, callback).await;
                if let Err(AspeakError::WebSocketError(TungsteniteError::Protocol(
                    ProtocolError::ResetWithoutClosingHandshake,
                ))) = result
                {
                    return result.with_note(|| "This error usually indicates a poor internet connection or that the remote API terminates your service.")
                        .with_suggestion(|| "Retry if you are on a poor internet connection. \
                                             If this error persists and you are using the trial service, please shorten your input.");
                } else {
                    result?;
                }
            }
            Command::ListVoices {
                ref voice,
                ref locale,
            } => {
                let url = "https://eastus.api.speech.microsoft.com/cognitiveservices/voices/list";
                let headers =
                    HeaderMap::from_iter([(header::ORIGIN, HeaderValue::from_str(ORIGIN).unwrap())]);
                let client = reqwest::ClientBuilder::new()
                    .default_headers(headers)
                    .build()
                    .unwrap();
                let request = client.get(url).build()?;
                let voices = client.execute(request).await?.json::<Vec<Voice>>().await?;
                let voices = voices.iter();
                let locale_id = locale.as_deref();
                let voice_id = voice.as_deref();
                let voices: Box<dyn Iterator<Item = &Voice>> = {
                    if locale_id.is_some() {
                        Box::new(voices.filter(|voice| Some(voice.locale.as_str()) == locale_id))
                    } else if voice_id.is_some() {
                        Box::new(voices.filter(|voice| Some(voice.short_name.as_str()) == voice_id))
                    } else {
                        Box::new(voices)
                    }
                };
                for voice in voices {
                    println!("{voice}");
                }
            }
            Command::ListQualities => {
                for (container, qualities) in QUALITY_MAP.into_iter() {
                    println!(
                        "{} {}:",
                        "Qualities for".cyan(),
                        container.to_uppercase().cyan()
                    );
                    for (quality, format) in qualities.into_iter() {
                        println!("{:>3}: {}", quality, Into::<&str>::into(format));
                    }
                    println!()
                }
            }
            Command::ListFormats => {
                for format in AudioFormat::iter() {
                    println!("{}", Into::<&str>::into(format));
                }
            }
            Command::Config { .. } => {
                let config: Config = toml::from_str(std::fs::read_to_string("src/cli/aspeak.toml")?.as_str())?;
                debug!("Config: {config:?}");
            }
        }
        Ok(())
    })?;
    Ok(())
}
