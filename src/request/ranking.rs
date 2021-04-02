use crate::{
    model::{
        ranking::{ChartRankings, CountryRankings, RankingType, Rankings, Spotlight},
        GameMode,
    },
    request::{Pending, Query, Request},
    routing::Route,
    Osu,
};

use futures::future::TryFutureExt;
use serde::Deserialize;

/// Get a [`ChartRankings`](crate::model::ranking::ChartRankings) struct
/// containing a [`Spotlight`](crate::model::ranking::Spotlight), its
/// [`Beatmapset`](crate::model::beatmap::Beatmapset)s, and participating
/// [`UserStatistics`](crate::model::user::UserStatistics).
///
/// The mapset will have their `maps` option filled.
///
/// The user statistics contain specific, spotlight related data.
/// All fields depends only on scores on maps of the spotlight.
/// The statistics vector is ordered by `ranked_score`.
/// The `user` option is filled.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct GetChartRankings<'a> {
    fut: Option<Pending<'a, ChartRankings>>,
    osu: &'a Osu,
    mode: GameMode,
    spotlight: Option<u32>,
}

impl<'a> GetChartRankings<'a> {
    #[inline]
    pub(crate) fn new(osu: &'a Osu, mode: GameMode) -> Self {
        Self {
            fut: None,
            osu,
            mode,
            spotlight: None,
        }
    }

    /// Specify the spotlight id. If none is given,
    /// the latest spotlight will be returned.
    #[inline]
    pub fn spotlight(mut self, spotlight_id: u32) -> Self {
        self.spotlight.replace(spotlight_id);

        self
    }

    fn start(&mut self) -> Pending<'a, ChartRankings> {
        #[cfg(feature = "metrics")]
        self.osu.metrics.chart_rankings.inc();

        let mut query = Query::new();

        if let Some(spotlight) = self.spotlight {
            query.push("spotlight", spotlight.to_string());
        }

        let req = Request::from((
            query,
            Route::GetRankings {
                mode: self.mode,
                ranking_type: RankingType::Charts,
            },
        ));

        Box::pin(self.osu.inner.request(req))
    }
}

poll_req!(GetChartRankings<'_> => ChartRankings);

/// Get a [`CountryRankings`](crate::model::ranking::CountryRankings) struct
/// containing a vec of [`CountryRanking`](crate::model::ranking::CountryRanking)s
/// which will be sorted by the country's total pp.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct GetCountryRankings<'a> {
    fut: Option<Pending<'a, CountryRankings>>,
    osu: &'a Osu,
    mode: GameMode,
    page: Option<u32>,
}

impl<'a> GetCountryRankings<'a> {
    #[inline]
    pub(crate) fn new(osu: &'a Osu, mode: GameMode) -> Self {
        Self {
            fut: None,
            osu,
            mode,
            page: None,
        }
    }

    #[inline]
    pub fn page(mut self, page: u32) -> Self {
        self.page.replace(page);

        self
    }

    fn start(&mut self) -> Pending<'a, CountryRankings> {
        #[cfg(feature = "metrics")]
        self.osu.metrics.country_rankings.inc();

        let mut query = Query::new();

        if let Some(page) = self.page {
            query.push("cursor[page]", page.to_string());
        }

        let req = Request::from((
            query,
            Route::GetRankings {
                mode: self.mode,
                ranking_type: RankingType::Country,
            },
        ));

        Box::pin(self.osu.inner.request(req))
    }
}

poll_req!(GetCountryRankings<'_> => CountryRankings);

/// Get a [`Rankings`](crate::model::ranking::Rankings) struct whose
/// [`UserStatistics`](crate::model::user::UserStatistics) are sorted
/// by their pp, i.e. the current pp leaderboard.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct GetPerformanceRankings<'a> {
    fut: Option<Pending<'a, Rankings>>,
    osu: &'a Osu,
    mode: GameMode,
    country: Option<String>,
    variant: Option<&'static str>,
    page: Option<u32>,
}

impl<'a> GetPerformanceRankings<'a> {
    #[inline]
    pub(crate) fn new(osu: &'a Osu, mode: GameMode) -> Self {
        Self {
            fut: None,
            osu,
            mode,
            country: None,
            variant: None,
            page: None,
        }
    }

    /// Specify a country code.
    #[inline]
    pub fn country(mut self, country: impl Into<String>) -> Self {
        self.country.replace(country.into());

        self
    }

    #[inline]
    pub fn variant_4k(mut self) -> Self {
        if self.mode == GameMode::MNA {
            self.variant.replace("4k");
        }

        self
    }

    #[inline]
    pub fn variant_7k(mut self) -> Self {
        if self.mode == GameMode::MNA {
            self.variant.replace("7k");
        }

        self
    }

    /// Pages range from 1 to 200.
    #[inline]
    pub fn page(mut self, page: u32) -> Self {
        self.page.replace(page);

        self
    }

    fn start(&mut self) -> Pending<'a, Rankings> {
        #[cfg(feature = "metrics")]
        self.osu.metrics.performance_rankings.inc();

        let mode = self.mode;
        let mut query = Query::new();

        if let Some(country) = self.country.take() {
            query.push("country", country);
        }

        if let Some(variant) = self.variant {
            query.push("variant", variant);
        }

        if let Some(page) = self.page {
            query.push("cursor[page]", page.to_string());
        }

        let req = Request::from((
            query,
            Route::GetRankings {
                mode,
                ranking_type: RankingType::Performance,
            },
        ));

        let fut = self
            .osu
            .inner
            .request(req)
            .map_ok(move |mut rankings: Rankings| {
                rankings.mode.replace(mode);
                rankings.ranking_type.replace(RankingType::Performance);

                rankings
            });

        Box::pin(fut)
    }
}

poll_req!(GetPerformanceRankings<'_> => Rankings);

/// Get a [`Rankings`](crate::model::ranking::Rankings) struct whose
/// [`UserStatistics`](crate::model::user::UserStatistics) are sorted
/// by their ranked score, i.e. the current ranked score leaderboard.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct GetScoreRankings<'a> {
    fut: Option<Pending<'a, Rankings>>,
    osu: &'a Osu,
    mode: GameMode,
    page: Option<u32>,
}

impl<'a> GetScoreRankings<'a> {
    #[inline]
    pub(crate) fn new(osu: &'a Osu, mode: GameMode) -> Self {
        Self {
            fut: None,
            osu,
            mode,
            page: None,
        }
    }

    /// Pages range from 1 to 200.
    #[inline]
    pub fn page(mut self, page: u32) -> Self {
        self.page.replace(page);

        self
    }

    fn start(&mut self) -> Pending<'a, Rankings> {
        #[cfg(feature = "metrics")]
        self.osu.metrics.score_rankings.inc();

        let mode = self.mode;
        let mut query = Query::new();

        if let Some(page) = self.page {
            query.push("cursor[page]", page.to_string());
        }

        let req = Request::from((
            query,
            Route::GetRankings {
                mode,
                ranking_type: RankingType::Score,
            },
        ));

        let fut = self
            .osu
            .inner
            .request(req)
            .map_ok(move |mut rankings: Rankings| {
                rankings.mode.replace(mode);
                rankings.ranking_type.replace(RankingType::Score);

                rankings
            });

        Box::pin(fut)
    }
}

poll_req!(GetScoreRankings<'_> => Rankings);

/// Get a vec of [`Spotlight`](crate::model::ranking::Spotlight)s.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct GetSpotlights<'a> {
    fut: Option<Pending<'a, Vec<Spotlight>>>,
    osu: &'a Osu,
}

impl<'a> GetSpotlights<'a> {
    #[inline]
    pub(crate) fn new(osu: &'a Osu) -> Self {
        Self { fut: None, osu }
    }

    fn start(&mut self) -> Pending<'a, Vec<Spotlight>> {
        #[cfg(feature = "metrics")]
        self.osu.metrics.spotlights.inc();

        let req = Request::from(Route::GetSpotlights);

        let fut = self
            .osu
            .inner
            .request(req)
            .map_ok(|s: Spotlights| s.spotlights);

        Box::pin(fut)
    }
}

poll_req!(GetSpotlights<'_> => Vec<Spotlight>);

#[derive(Deserialize)]
struct Spotlights {
    spotlights: Vec<Spotlight>,
}
