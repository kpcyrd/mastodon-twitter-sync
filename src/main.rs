use mammut::{Mastodon, StatusesRequest};
use std::fs::File;
use std::io::prelude::*;
use std::process;
use tokio::runtime::current_thread::block_on_all;

use crate::config::*;
use crate::delete_favs::*;
use crate::delete_statuses::mastodon_delete_older_statuses;
use crate::delete_statuses::twitter_delete_older_statuses;
use crate::post::*;
use crate::registration::mastodon_register;
use crate::registration::twitter_register;
use crate::sync::*;

mod config;
mod delete_favs;
mod delete_statuses;
mod post;
mod registration;
mod sync;

fn main() {
    let config = match File::open("mastodon-twitter-sync.toml") {
        Ok(f) => config_load(f),
        Err(_) => {
            let mastodon = mastodon_register();
            let twitter_config = twitter_register();
            let config = Config {
                mastodon: MastodonConfig {
                    app: (*mastodon).clone(),
                    // Do not delete older status per default, users should
                    // enable this explicitly.
                    delete_older_statuses: false,
                    delete_older_favs: false,
                },
                twitter: twitter_config,
            };

            // Save config for using on the next run.
            let toml = toml::to_string(&config).unwrap();
            let mut file = File::create("mastodon-twitter-sync.toml").unwrap();
            file.write_all(toml.as_bytes()).unwrap();

            config
        }
    };

    let mastodon = Mastodon::from_data(config.mastodon.app);

    let account = match mastodon.verify_credentials() {
        Ok(account) => account,
        Err(e) => {
            println!("Error connecting to Mastodon: {:#?}", e);
            process::exit(1);
        }
    };
    // Get most recent toots but without replies.
    let mastodon_statuses =
        match mastodon.statuses(&account.id, StatusesRequest::new().exclude_replies()) {
            Ok(statuses) => statuses.initial_items,
            Err(e) => {
                println!("Error fetching toots from Mastodon: {:#?}", e);
                process::exit(2);
            }
        };

    let con_token =
        egg_mode::KeyPair::new(config.twitter.consumer_key, config.twitter.consumer_secret);
    let access_token = egg_mode::KeyPair::new(
        config.twitter.access_token,
        config.twitter.access_token_secret,
    );
    let token = egg_mode::Token::Access {
        consumer: con_token,
        access: access_token,
    };

    let timeline = egg_mode::tweet::user_timeline(config.twitter.user_id, false, true, &token)
        .with_page_size(50);

    let (timeline, first_tweets) = match block_on_all(timeline.start()) {
        Ok(tweets) => tweets,
        Err(e) => {
            println!("Error fetching tweets from Twitter: {:#?}", e);
            process::exit(3);
        }
    };
    let mut tweets = (*first_tweets).to_vec();
    // We might have only one tweet because of filtering out reply tweets. Fetch
    // some more tweets to make sure we have enough for comparing.
    if tweets.len() < 50 {
        let (_, mut next_tweets) = match block_on_all(timeline.older(None)) {
            Ok(tweets) => tweets,
            Err(e) => {
                println!("Error fetching older tweets from Twitter: {:#?}", e);
                process::exit(4);
            }
        };
        tweets.append(&mut (*next_tweets).to_vec());
    }
    let mut posts = determine_posts(&mastodon_statuses, &tweets);

    posts = filter_posted_before(posts);

    for toot in posts.toots {
        println!("Posting to Mastodon: {}", toot.text);
        if let Err(e) = post_to_mastodon(&mastodon, toot) {
            println!("Error posting toot to Mastodon: {:#?}", e);
            process::exit(5);
        }
    }

    for tweet in posts.tweets {
        println!("Posting to Twitter: {}", tweet.text);
        if let Err(e) = post_to_twitter(&token, tweet) {
            println!("Error posting tweet to Twitter: {:#?}", e);
            process::exit(6);
        }
    }

    // Delete old mastodon statuses if that option is enabled.
    if config.mastodon.delete_older_statuses {
        mastodon_delete_older_statuses(&mastodon, &account);
    }
    if config.twitter.delete_older_statuses {
        twitter_delete_older_statuses(config.twitter.user_id, &token);
    }

    // Delete old mastodon favourites if that option is enabled.
    if config.mastodon.delete_older_favs {
        mastodon_delete_older_favs(&mastodon);
    }
    if config.twitter.delete_older_favs {
        twitter_delete_older_favs(config.twitter.user_id, &token);
    }
}
