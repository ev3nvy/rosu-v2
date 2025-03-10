mod builder;
mod token;

use bytes::Bytes;
use token::{Authorization, AuthorizationKind, Token, TokenResponse};

pub use builder::OsuBuilder;
pub use token::Scope;

use crate::{error::OsuError, model::GameMode, request::*, OsuResult};

use hyper::{
    body::{Body as HyperBody, HttpBody, SizeHint},
    client::{Client as HyperClient, HttpConnector},
    header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT},
    HeaderMap, Method, Request as HyperRequest, Response, StatusCode,
};
use hyper_rustls::HttpsConnector;
use leaky_bucket_lite::LeakyBucket;
use serde::de::DeserializeOwned;
use std::{
    convert::Infallible,
    mem,
    ops::Drop,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::{oneshot::Sender, RwLock};
use url::Url;

#[cfg(feature = "cache")]
use {crate::prelude::Username, dashmap::DashMap};

#[cfg(feature = "metrics")]
use {crate::metrics::Metrics, prometheus::IntCounterVec};

/// The main osu client.
/// Cheap to clone.
pub struct Osu {
    pub(crate) inner: Arc<OsuRef>,
    #[cfg(feature = "cache")]
    pub(crate) cache: Arc<DashMap<Username, u32>>,
    #[cfg(feature = "metrics")]
    pub(crate) metrics: Arc<Metrics>,
    token_loop_tx: Option<Sender<()>>,
}

impl Osu {
    /// Create a new default [`Osu`](crate::Osu) client.
    ///
    /// Errors if the API did not provide a token for the given client id and client secret.
    pub async fn new(client_id: u64, client_secret: impl Into<String>) -> OsuResult<Self> {
        Self::builder()
            .client_id(client_id)
            .client_secret(client_secret)
            .build()
            .await
    }

    /// Fine-tune building an [`Osu`](crate::Osu) client.
    #[inline]
    pub fn builder() -> OsuBuilder {
        OsuBuilder::default()
    }

    /// Returns an [`IntCounterVec`](crate::prelude::IntCounterVec) from
    /// [prometheus](https://crates.io/crates/prometheus) containing
    /// a counter for each request type.
    #[cfg(feature = "metrics")]
    pub fn metrics(&self) -> IntCounterVec {
        self.metrics.counters.clone()
    }

    /// Get a [`Beatmap`](crate::model::beatmap::Beatmap).
    ///
    /// Filled options will be: `deleted_at` (if deleted), `fail_times`,
    /// `mapset` and `max_combo` (if available for mode).
    ///
    /// The contained [`Beatmapset`](crate::model::beatmap::Beatmapset) will
    /// have these options filled: `legacy_thread_url`, `ratings`,
    /// `ranked_date` (if not unranked) and `submitted_date` (if submitted).
    #[inline]
    pub fn beatmap(&self) -> GetBeatmap<'_> {
        GetBeatmap::new(self)
    }

    /// Get a vec of at most 50 [`BeatmapCompact`](crate::model::beatmap::BeatmapCompact)s.
    ///
    /// The contained maps will have these options filled: `mapset`,
    /// `fail_times`, and `max_combo` (if available for mode).
    #[inline]
    pub fn beatmaps<I>(&self, map_ids: I) -> GetBeatmaps<'_>
    where
        I: IntoIterator<Item = u32>,
    {
        GetBeatmaps::new(self, map_ids)
    }

    /// Get a vec of [`Score`](crate::model::score::Score).
    ///
    /// The contained scores will have the following options filled:
    /// `pp` (if ranked or approved), and `user`.
    ///
    /// The scores' contained [`UserCompact`](crate::model::user::UserCompact)
    /// will have the `country` and `cover` options filled.
    #[inline]
    pub fn beatmap_scores(&self, map_id: u32) -> GetBeatmapScores<'_> {
        GetBeatmapScores::new(self, map_id)
    }

    /// Get the [`BeatmapDifficultyAttributes`](crate::model::beatmap::BeatmapDifficultyAttributes) for a map.
    #[inline]
    pub fn beatmap_difficulty_attributes(&self, map_id: u32) -> GetBeatmapDifficultyAttributes<'_> {
        GetBeatmapDifficultyAttributes::new(self, map_id)
    }

    /// Get a [`BeatmapUserScore`](crate::model::score::BeatmapUserScore).
    ///
    /// The contained [`Score`](crate::model::score::Score) will have the
    /// `map` and `user` options filled.
    #[cfg(not(feature = "cache"))]
    #[inline]
    pub fn beatmap_user_score(&self, map_id: u32, user_id: u32) -> GetBeatmapUserScore<'_> {
        GetBeatmapUserScore::new(self, map_id, user_id)
    }

    /// Get the top score for each mod combination a user has on a
    /// map in form of a vec of [`Score`](crate::model::score::Score)s.
    ///
    /// The contained scores won't have any Options filled except
    /// for `pp` in case of a ranked map.
    #[cfg(not(feature = "cache"))]
    #[inline]
    pub fn beatmap_user_scores(&self, map_id: u32, user_id: u32) -> GetBeatmapUserScores<'_> {
        GetBeatmapUserScores::new(self, map_id, user_id)
    }

    /// Get a [`BeatmapUserScore`](crate::model::score::BeatmapUserScore).
    ///
    /// The contained [`Score`](crate::model::score::Score) will have the
    /// `map` and `user` options filled.
    #[cfg(feature = "cache")]
    #[inline]
    pub fn beatmap_user_score(
        &self,
        map_id: u32,
        user_id: impl Into<UserId>,
    ) -> GetBeatmapUserScore<'_> {
        GetBeatmapUserScore::new(self, map_id, user_id.into())
    }

    /// Get the top score for each mod combination a user has on a
    /// map in form of a vec of [`Score`](crate::model::score::Score)s.
    ///
    /// The contained scores won't have any Options filled except
    /// for `pp` in case of a ranked map.
    #[cfg(feature = "cache")]
    #[inline]
    pub fn beatmap_user_scores(
        &self,
        map_id: u32,
        user_id: impl Into<UserId>,
    ) -> GetBeatmapUserScores<'_> {
        GetBeatmapUserScores::new(self, map_id, user_id.into())
    }

    /// Get a [`Beatmapset`](crate::model::beatmap::Beatmapset).
    ///
    /// Filled options will be: `artist_unicode`, `converts`, `description`,
    /// `genre`, `language`, `legacy_thread_url`, `maps`, `ratings`,
    /// `ranked_date` (if not unranked), `recent_favourites`,
    /// `submitted_date` (if submitted), and `title_unicode`.
    ///
    /// The contained [`Beatmap`](crate::model::beatmap::Beatmap)s
    /// will contain `Some` in `fail_times`, `max_combo`
    /// (if available for mode), and `deleted_at` (if deleted).
    #[inline]
    pub fn beatmapset(&self, mapset_id: u32) -> GetBeatmapset<'_> {
        GetBeatmapset::new(self, mapset_id)
    }

    /// Get a [`Beatmapset`](crate::model::beatmap::Beatmapset) from a map ID.
    ///
    /// Filled options will be: `artist_unicode`, `converts`, `description`,
    /// `genre`, `language`, `legacy_thread_url`, `maps`, `ratings`,
    /// `ranked_date` (if not unranked), `recent_favourites`,
    /// `submitted_date` (if submitted), and `title_unicode`.
    ///
    /// The contained [`Beatmap`](crate::model::beatmap::Beatmap)s
    /// will contain `Some` in `fail_times`, `max_combo`
    /// (if available for mode), and `deleted_at` (if deleted).
    #[inline]
    pub fn beatmapset_from_map_id(&self, map_id: u32) -> GetBeatmapsetFromMapId<'_> {
        GetBeatmapsetFromMapId::new(self, map_id)
    }

    /// Get a [`BeatmapsetEvents`](crate::model::beatmap::BeatmapsetEvents)
    /// struct containing the most recent mapset events.
    #[inline]
    pub fn beatmapset_events(&self) -> GetBeatmapsetEvents<'_> {
        GetBeatmapsetEvents::new(self)
    }

    /// Get a [`BeatmapsetSearchResult`](crate::model::beatmap::BeatmapsetSearchResult)
    /// struct containing the first page of maps that fit the search query.
    ///
    /// The default search parameters are:
    /// - mode: any
    /// - status: has leaderboard (ranked, loved, approved, and qualified)
    /// - genre: any
    /// - language: any
    /// - extra: contains neither video nor storyboard
    /// - nsfw: allowed
    /// - sort: by relevance, descending
    ///
    /// The contained [`Beatmapset`](crate::model::beatmap::Beatmapset)s will have the
    /// following options filled: `artist_unicode`, `legacy_thread_url`, `maps`,
    /// `ranked_date` and `submitted_date` if available, and `title_unicode`.
    ///
    /// The search query allows the following options to be specified: `ar`, `artist`,
    /// `bpm`, `created`, `creator`, `cs`, `dr` (hp drain rate), `keys`, `length`,
    /// `ranked`, `stars`, and `status`.
    ///
    /// ## Example
    ///
    /// ```
    /// // Search for mapsets from Sotarks that have a map with no more than AR 9.
    /// let query = "creator=sotarks ar<9";
    ///
    /// // Loved mapsets from Camellia including at least one map above 8 stars
    /// let query = "status=loved artist=camellia stars>8";
    /// ```
    #[inline]
    pub fn beatmapset_search(&self) -> GetBeatmapsetSearch<'_> {
        GetBeatmapsetSearch::new(self)
    }

    /// Get a list of comments and their replies up to two levels deep
    /// in form of a [`CommentBundle`](crate::model::comments::CommentBundle) .
    #[inline]
    pub fn comments(&self) -> GetComments<'_> {
        GetComments::new(self)
    }

    /// Get a [`ChartRankings`](crate::model::ranking::ChartRankings) struct
    /// containing a [`Spotlight`](crate::model::ranking::Spotlight), its
    /// [`Beatmapset`](crate::model::beatmap::Beatmapset)s, and participating
    /// [`UserCompact`](crate::model::user::UserCompact).
    ///
    /// The mapset will have their `maps` option filled.
    ///
    /// The user statistics contain specific, spotlight related data.
    /// All fields depends only on scores on maps of the spotlight.
    /// The statistics vector is ordered by `ranked_score`.
    /// The `user` option is filled.
    #[inline]
    pub fn chart_rankings(&self, mode: GameMode) -> GetChartRankings<'_> {
        GetChartRankings::new(self, mode)
    }

    /// Get a [`CountryRankings`](crate::model::ranking::CountryRankings) struct
    /// containing a vec of [`CountryRanking`](crate::model::ranking::CountryRanking)s
    /// which will be sorted by the country's total pp.
    #[inline]
    pub fn country_rankings(&self, mode: GameMode) -> GetCountryRankings<'_> {
        GetCountryRankings::new(self, mode)
    }

    /// Get a [`ForumPosts`](crate::model::forum::ForumPosts) struct for a forum topic
    #[inline]
    pub fn forum_posts(&self, topic_id: u64) -> GetForumPosts<'_> {
        GetForumPosts::new(self, topic_id)
    }

    /// Get the kudosu history of a user in form of a vec of
    /// [`KudosuHistory`](crate::model::kudosu::KudosuHistory).
    #[cfg(not(feature = "cache"))]
    #[inline]
    pub fn kudosu(&self, user_id: u32) -> GetUserKudosu<'_> {
        GetUserKudosu::new(self, user_id)
    }

    /// Get the kudosu history of a user in form of a vec of
    /// [`KudosuHistory`](crate::model::kudosu::KudosuHistory).
    #[cfg(feature = "cache")]
    #[inline]
    pub fn kudosu(&self, user_id: impl Into<UserId>) -> GetUserKudosu<'_> {
        GetUserKudosu::new(self, user_id.into())
    }

    /// Get [`News`](crate::model::news::News).
    #[inline]
    pub fn news(&self) -> GetNews<'_> {
        GetNews::new(self)
    }

    /// Get an [`OsuMatch`](crate::model::matches::OsuMatch).
    #[inline]
    pub fn osu_match(&self, match_id: u32) -> GetMatch<'_> {
        GetMatch::new(self, match_id)
    }

    /// Get a [`MatchList`](crate::model::matches::MatchList) containing all
    /// currently open multiplayer lobbies.
    #[inline]
    pub fn osu_matches(&self) -> GetMatches<'_> {
        GetMatches::new(self)
    }

    /// Get the [`User`](crate::model::user::User) of the authenticated user.
    ///
    /// Note that the client has to be initialized with the `identify` scope
    /// through the OAuth process in order for this endpoint to not return an error.
    ///
    /// See [`OsuBuilder::with_authorization`](crate::OsuBuilder::with_authorization).
    #[inline]
    pub fn own_data(&self) -> GetOwnData<'_> {
        GetOwnData::new(self)
    }

    /// Get a [`Rankings`](crate::model::ranking::Rankings) struct whose
    /// [`UserCompact`](crate::model::user::UserCompact)s are sorted
    /// by their pp, i.e. the current pp leaderboard.
    #[inline]
    pub fn performance_rankings(&self, mode: GameMode) -> GetPerformanceRankings<'_> {
        GetPerformanceRankings::new(self, mode)
    }

    /// Get the recent activity of a user in form of a vec of
    /// [`RecentEvent`](crate::model::recent_event::RecentEvent)s.
    #[cfg(not(feature = "cache"))]
    #[inline]
    pub fn recent_events(&self, user_id: u32) -> GetRecentEvents<'_> {
        GetRecentEvents::new(self, user_id)
    }

    /// Get the recent activity of a user in form of a vec of
    /// [`RecentEvent`](crate::model::recent_event::RecentEvent)s.
    #[cfg(feature = "cache")]
    #[inline]
    pub fn recent_events(&self, user_id: impl Into<UserId>) -> GetRecentEvents<'_> {
        GetRecentEvents::new(self, user_id.into())
    }

    /// Get the replay of a score in form of a [`Replay`](osu_db::Replay).
    ///
    /// Note that the client has to be initialized through the OAuth process
    /// in order for this endpoint to not return an error.
    ///
    /// See [`OsuBuilder::with_authorization`](crate::OsuBuilder::with_authorization).
    #[cfg(feature = "replay")]
    #[inline]
    pub fn replay(&self, mode: GameMode, score_id: u64) -> GetReplay<'_> {
        GetReplay::new(self, mode, score_id)
    }

    /// Get the bytes of a replay of a score in form of a `Vec<u8>`.
    ///
    /// Note that the client has to be initialized through the OAuth process
    /// in order for this endpoint to not return an error.
    ///
    /// See [`OsuBuilder::with_authorization`](crate::OsuBuilder::with_authorization).
    #[inline]
    pub fn replay_raw(&self, mode: GameMode, score_id: u64) -> GetReplayRaw<'_> {
        GetReplayRaw::new(self, mode, score_id)
    }

    /// Get a [`Score`](crate::model::score::Score) struct.
    ///
    /// The contained score will have the following options filled:
    /// `map` (will contain `checksum` and `max_combo`), `mapset`
    /// (will contain `artist_unicode` and `title_unicode`), `pp`
    /// (if ranked), `rank_global` (if on leaderboard map) and `user`
    /// (will contain `last_visited`, `country`, `cover` and `groups`)
    #[inline]
    pub fn score(&self, score_id: u64, mode: GameMode) -> GetScore<'_> {
        GetScore::new(self, score_id, mode)
    }

    /// Get a [`Rankings`](crate::model::ranking::Rankings) struct whose
    /// [`UserCompact`](crate::model::user::UserCompact)s are sorted
    /// by their ranked score, i.e. the current ranked score leaderboard.
    #[inline]
    pub fn score_rankings(&self, mode: GameMode) -> GetScoreRankings<'_> {
        GetScoreRankings::new(self, mode)
    }

    /// Get [`SeasonalBackgrounds`](crate::model::seasonal_backgrounds::SeasonalBackgrounds).
    #[inline]
    pub fn seasonal_backgrounds(&self) -> GetSeasonalBackgrounds<'_> {
        GetSeasonalBackgrounds::new(self)
    }

    /// Get the vec of [`Spotlight`](crate::model::ranking::Spotlight).
    #[inline]
    pub fn spotlights(&self) -> GetSpotlights<'_> {
        GetSpotlights::new(self)
    }

    /// Get a [`User`](crate::model::user::User).
    ///
    /// The following options will be filled if the user specified them:
    /// `discord`, `interests`, `location`, `occupation`, `playstyle`,
    /// `profile_color`, `skype`, `title`, `title_url`, `website`
    ///
    /// The only `is_*` options that will be filled are `is_active`, `is_bot`,
    /// `is_deleted`, `is_online`, and `is_supporter`, the others won't be.
    ///
    /// All other options will be filled.
    #[inline]
    pub fn user(&self, user_id: impl Into<UserId>) -> GetUser<'_> {
        GetUser::new(self, user_id)
    }

    /// Get the [`Beatmapset`](crate::model::beatmap::Beatmapset)s of a user by their id.
    ///
    /// If no map type specified, either manually through
    /// [`map_type`](crate::request::GetUserBeatmapsets::map_type),
    /// or through any of the methods [`loved`](crate::request::GetUserBeatmapsets::loved),
    /// [`favourite`](crate::request::GetUserBeatmapsets::favourite),
    /// [`graveyard`](crate::request::GetUserBeatmapsets::graveyard),
    /// [`ranked_and_approved`](crate::request::GetUserBeatmapsets::ranked_and_approved),
    /// [`unranked`](crate::request::GetUserBeatmapsets::unranked),
    /// it defaults to `ranked_and_approved`.
    ///
    /// Filled options will be: `artist_unicode`, `legacy_thread_url`, `maps`, `title_unicode`.
    ///
    /// All options of the contained [`Beatmap`](crate::model::beatmap::Beatmap)s will be `None`.
    #[cfg(not(feature = "cache"))]
    #[inline]
    pub fn user_beatmapsets(&self, user_id: u32) -> GetUserBeatmapsets<'_> {
        GetUserBeatmapsets::new(self, user_id)
    }

    /// Get a vec of [`Beatmapset`](crate::model::beatmap::Beatmapset)s a user made.
    ///
    /// Filled options will be: `artist_unicode`, `legacy_thread_url`, `maps`, `title_unicode`.
    ///
    /// All options of the contained [`Beatmap`](crate::model::beatmap::Beatmap)s will be `None`.
    #[cfg(feature = "cache")]
    #[inline]
    pub fn user_beatmapsets(&self, user_id: impl Into<UserId>) -> GetUserBeatmapsets<'_> {
        GetUserBeatmapsets::new(self, user_id.into())
    }

    /// Get a vec of a user's [`MostPlayedMap`](crate::model::beatmap::MostPlayedMap)s.
    ///
    /// All options of the contained [`BeatmapCompact`](crate::model::beatmap::BeatmapCompact) and
    /// [`BeatmapsetCompact`](crate::model::beatmap::BeatmapsetCompact) will be `None`.
    ///
    /// ## Limit
    ///
    /// The API provides at most 100 results, defaults to 5.
    #[cfg(not(feature = "cache"))]
    #[inline]
    pub fn user_most_played(&self, user_id: u32) -> GetUserMostPlayed<'_> {
        GetUserMostPlayed::new(self, user_id)
    }

    /// Get a vec of a user's [`MostPlayedMap`](crate::model::beatmap::MostPlayedMap)s.
    ///
    /// All options of the contained [`BeatmapCompact`](crate::model::beatmap::BeatmapCompact) and
    /// [`BeatmapsetCompact`](crate::model::beatmap::BeatmapsetCompact) will be `None`.
    ///
    /// ## Limit
    ///
    /// The API provides at most 100 results, defaults to 5.
    #[cfg(feature = "cache")]
    #[inline]
    pub fn user_most_played(&self, user_id: impl Into<UserId>) -> GetUserMostPlayed<'_> {
        GetUserMostPlayed::new(self, user_id.into())
    }

    /// Get either top, global firsts, pinned, or recent scores of a user,
    /// i.e. a vec of [`Score`](crate::model::score::Score).
    ///
    /// If no score type is specified by either
    /// [`best`](crate::request::GetUserScores::best),
    /// [`firsts`](crate::request::GetUserScores::firsts),
    /// [`pinned`](crate::request::GetUserScores::pinned),
    /// or [`recent`](crate::request::GetUserScores::recent), it defaults to `best`.
    ///
    /// The resulting scores will have these options filled: `map`, `mapset`, `pp`, `user`
    ///
    /// Additionally, the `best` score type will provide the `weight` option.
    ///
    /// All options of the contained [`Beatmap`](crate::model::beatmap::Beatmap),
    /// [`BeatmapsetCompact`](crate::model::beatmap::Beatmapset), and
    /// [`UserCompact`](crate::model::user::UserCompact) will be `None`.
    ///
    /// ## Note
    ///
    /// - The API provides at most 100 results per requests and defaults to 5.
    /// - For the `recent` score type, failed score are excluded by default.
    /// Use [`include_fails`](crate::request::GetUserScores::include_fails)
    /// to include them.
    /// - For the `firsts` score type, `pp` will only be `Some` if the map
    /// is not loved.
    #[cfg(not(feature = "cache"))]
    #[inline]
    pub fn user_scores(&self, user_id: u32) -> GetUserScores<'_> {
        GetUserScores::new(self, user_id)
    }

    /// Get either top, global firsts, pinned, or recent scores of a user,
    /// i.e. a vec of [`Score`](crate::model::score::Score).
    ///
    /// If no score type is specified by either
    /// [`best`](crate::request::GetUserScores::best),
    /// [`firsts`](crate::request::GetUserScores::firsts),
    /// [`pinned`](crate::request::GetUserScores::pinned),
    /// or [`recent`](crate::request::GetUserScores::recent), it defaults to `best`.
    ///
    /// The resulting scores will have these options filled: `map`, `mapset`, `pp`, `user`
    ///
    /// Additionally, the `best` score type will provide the `weight` option.
    ///
    /// All options of the contained [`Beatmap`](crate::model::beatmap::Beatmap),
    /// [`BeatmapsetCompact`](crate::model::beatmap::Beatmapset), and
    /// [`UserCompact`](crate::model::user::UserCompact) will be `None`.
    ///
    /// ## Note
    ///
    /// - The API provides at most 100 results per requests and defaults to 5.
    /// - For the `recent` score type, failed score are excluded by default.
    /// Use [`include_fails`](crate::request::GetUserScores::include_fails)
    /// to include them.
    /// - For the `firsts` score type, `pp` will only be `Some` if the map
    /// is not loved.
    #[cfg(feature = "cache")]
    #[inline]
    pub fn user_scores(&self, user_id: impl Into<UserId>) -> GetUserScores<'_> {
        GetUserScores::new(self, user_id.into())
    }

    /// Get a vec of [`UserCompact`](crate::model::user::UserCompact).
    #[deprecated = "The API currently doesn't allow this endpoint for public use"]
    #[inline]
    pub fn users(&self, user_ids: &[u32]) -> GetUsers<'_> {
        GetUsers::new(self, user_ids)
    }

    /// Get a [`WikiPage`](crate::model::wiki::WikiPage) or image data.
    ///
    /// `locale` adjusts the language, e.g. `en` for english, `de` for german, ...
    #[inline]
    pub fn wiki(&self, locale: impl Into<String>) -> GetWikiPage<'_> {
        GetWikiPage::new(self, locale)
    }

    #[cfg(feature = "cache")]
    pub(crate) async fn cache_user(&self, user_id: UserId) -> OsuResult<u32> {
        match user_id {
            UserId::Id(id) => Ok(id),
            UserId::Name(mut name) => {
                // osu! usernames are ASCII-only
                name.make_ascii_lowercase();

                if let Some(id) = self.cache.get(&name) {
                    return Ok(*id.value());
                }

                let user = self.user(UserId::Name(name.clone())).await?;
                self.cache.insert(name, user.user_id);

                #[cfg(feature = "metrics")]
                // ! BUG: It's possible to increment twice for the same name due to
                // ! concurrent function calls but since `DashMap::len` is a non-trivial
                // ! method to call and `cache_user` is called frequently, it's hopefully
                // ! fine to just naively increment here and ignore double-countings.
                self.metrics.cache_size.inc();

                Ok(user.user_id)
            }
        }
    }

    #[cfg(feature = "cache")]
    pub(crate) fn update_cache(&self, user_id: u32, username: &Username) {
        let mut name = username.to_owned();
        name.make_ascii_lowercase();
        self.cache.insert(name, user_id);
    }

    pub(crate) async fn request<T: DeserializeOwned>(&self, req: Request) -> OsuResult<T> {
        self.inner.request(req).await
    }

    pub(crate) async fn request_raw(&self, req: Request) -> OsuResult<Bytes> {
        self.inner.request_raw(req).await
    }
}

impl Drop for Osu {
    #[inline]
    fn drop(&mut self) {
        if let Some(tx) = self.token_loop_tx.take() {
            let _ = tx.send(());
        }
    }
}

pub(crate) struct OsuRef {
    client_id: u64,
    client_secret: String,
    http: HyperClient<HttpsConnector<HttpConnector>, BodyBytes>,
    timeout: Duration,
    ratelimiter: LeakyBucket,
    auth_kind: AuthorizationKind,
    token: RwLock<Token>,
    retries: usize,
}

static MY_USER_AGENT: &str = concat!(
    "Rust API v2 (",
    env!("CARGO_PKG_REPOSITORY"),
    " v",
    env!("CARGO_PKG_VERSION"),
    ")",
);

const APPLICATION_JSON: &str = "application/json";
const X_API_VERSION: &str = "x-api-version";

const API_VERSION: u32 = 20220705;

impl OsuRef {
    async fn request_token(&self) -> OsuResult<TokenResponse> {
        let mut body = Body::default();
        body.push_without_quotes("client_id", self.client_id);
        body.push_with_quotes("client_secret", &self.client_secret);

        match &self.auth_kind {
            AuthorizationKind::Client(scope) => {
                body.push_with_quotes("grant_type", "client_credentials");
                body.push_with_quotes("scope", scope);
            }
            AuthorizationKind::User(auth) => match &self.token.read().await.refresh {
                Some(refresh) => {
                    body.push_with_quotes("grant_type", "refresh_token");
                    body.push_with_quotes("refresh_token", refresh);
                }
                None => {
                    body.push_with_quotes("grant_type", "authorization_code");
                    body.push_with_quotes("redirect_uri", &auth.redirect_uri);
                    body.push_with_quotes("code", &auth.code);
                    // FIXME: let users decide which scopes to use?
                    body.push_with_quotes("scope", "identify public");
                }
            },
        };

        let bytes = BodyBytes::from(body);
        let url = "https://osu.ppy.sh/oauth/token";

        let req = HyperRequest::builder()
            .method(Method::POST)
            .uri(url)
            .header(USER_AGENT, MY_USER_AGENT)
            .header(ACCEPT, APPLICATION_JSON)
            .header(CONTENT_TYPE, APPLICATION_JSON)
            .header(CONTENT_LENGTH, bytes.len())
            .body(bytes)?;

        let resp = self.send_request(req).await?;
        let bytes = self.handle_status(resp).await?;

        parse_bytes(bytes)
    }

    async fn request<T: DeserializeOwned>(&self, req: Request) -> OsuResult<T> {
        let bytes = self.request_raw(req).await?;

        // let text = String::from_utf8_lossy(&bytes);
        // println!("Response:\n{}", text);

        parse_bytes(bytes)
    }

    async fn request_raw(&self, req: Request) -> OsuResult<Bytes> {
        let resp = self.raw(req).await?;
        let bytes = self.handle_status(resp).await?;

        Ok(bytes)
    }

    async fn raw(&self, req: Request) -> OsuResult<Response<HyperBody>> {
        let Request {
            query,
            method,
            path,
            body,
        } = req;

        let url = format!("https://osu.ppy.sh/api/v2/{}{}", path, query);
        let url = Url::parse(&url).map_err(|source| OsuError::Url { source, url })?;
        debug!("URL: {}", url);

        if let Some(ref token) = self.token.read().await.access {
            let value = HeaderValue::from_str(token)
                .map_err(|source| OsuError::CreatingTokenHeader { source })?;

            let bytes = BodyBytes::from(body);

            let mut req_builder = HyperRequest::builder()
                .method(method)
                .uri(url.as_str())
                .header(AUTHORIZATION, value)
                .header(USER_AGENT, MY_USER_AGENT)
                .header(X_API_VERSION, API_VERSION)
                .header(ACCEPT, APPLICATION_JSON)
                .header(CONTENT_LENGTH, bytes.len());

            if !bytes.is_empty() {
                req_builder = req_builder.header(CONTENT_TYPE, APPLICATION_JSON);
            }

            let req = req_builder.body(bytes)?;

            self.send_request(req).await
        } else {
            Err(OsuError::NoToken)
        }
    }

    async fn send_request(&self, req: HyperRequest<BodyBytes>) -> OsuResult<Response<HyperBody>> {
        self.ratelimiter.acquire_one().await;

        let mut attempt = 0;

        loop {
            let req = clone_req(&req);

            match tokio::time::timeout(self.timeout, self.http.request(req)).await {
                Ok(res) => return res.map_err(|source| OsuError::Request { source }),
                Err(_) if attempt < self.retries => {
                    warn!("Timed out on attempt {attempt}, retry...");
                    attempt += 1;
                }
                Err(_) => return Err(OsuError::RequestTimeout),
            }
        }
    }

    async fn handle_status(&self, resp: Response<HyperBody>) -> OsuResult<Bytes> {
        let status = resp.status();

        let bytes = hyper::body::to_bytes(resp.into_body())
            .await
            .map_err(|source| OsuError::ChunkingResponse { source })?;

        match status {
            StatusCode::OK => return Ok(bytes),
            StatusCode::NOT_FOUND => return Err(OsuError::NotFound),
            StatusCode::SERVICE_UNAVAILABLE => {
                let body = String::from_utf8_lossy(&bytes).into_owned();

                return Err(OsuError::ServiceUnavailable(body));
            }
            StatusCode::TOO_MANY_REQUESTS => warn!("Got a 429 response"),
            _ => {}
        }

        let body = String::from_utf8_lossy(&bytes).into_owned();

        let source = match serde_json::from_slice(&bytes) {
            Ok(source) => source,
            Err(source) => return Err(OsuError::Parsing { body, source }),
        };

        Err(OsuError::Response {
            body,
            source,
            status,
        })
    }
}

#[inline]
fn parse_bytes<T: DeserializeOwned>(bytes: Bytes) -> OsuResult<T> {
    serde_json::from_slice(&bytes).map_err(|source| {
        let body = String::from_utf8_lossy(&bytes).into_owned();

        OsuError::Parsing { body, source }
    })
}

fn clone_req(req: &HyperRequest<BodyBytes>) -> HyperRequest<BodyBytes> {
    let mut builder = HyperRequest::builder().method(req.method()).uri(req.uri());

    if let Some(headers) = builder.headers_mut() {
        *headers = req.headers().to_owned();
    }

    builder.body(req.body().to_owned()).unwrap()
}

/// `hyper` requires the `HttpBody` trait to be implemented for the type that
/// requests are generic over. Since that trait is not implemented for `Bytes`
/// in hyper itself, we define a simple wrapper for which we implement the trait
/// the same way it is implemented for `String` in hyper.
#[derive(Clone, Default)]
struct BodyBytes(Bytes);

impl BodyBytes {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn len(&self) -> usize {
        self.0.len()
    }
}

impl HttpBody for BodyBytes {
    type Data = Bytes;
    type Error = Infallible;

    #[inline]
    fn poll_data(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        if !self.is_empty() {
            let bytes = mem::take(&mut self.0);

            Poll::Ready(Some(Ok(bytes)))
        } else {
            Poll::Ready(None)
        }
    }

    #[inline]
    fn poll_trailers(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Option<HeaderMap>, Self::Error>> {
        Poll::Ready(Ok(None))
    }

    #[inline]
    fn is_end_stream(&self) -> bool {
        self.is_empty()
    }

    #[inline]
    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.len() as u64)
    }
}

impl From<Body> for BodyBytes {
    #[inline]
    fn from(body: Body) -> Self {
        Self(body.into_bytes().into())
    }
}
