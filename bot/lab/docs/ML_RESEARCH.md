# ML research design

The project uses “scikit-learn-style” tabular ML patterns, implemented in Rust so scheduled training has no Python environment or binary-model dependency. This is a research pipeline, not a profit guarantee.

## What the training cycles use

- Regularised logistic regression is the transparent classification baseline.
- Ridge regression predicts cross-sectional next-session excess return and selects regularisation only on older data.
- Histogram gradient-boosted trees model nonlinear entry outcomes, with early stopping selected inside the training period.
- A block-bagged feature-subspace ensemble now trains multiple boosted models on deterministic bootstraps of complete time blocks and random 70% feature subsets. Averaging reduces model variance; sampling complete blocks preserves shared market dependence.
- Residual networks and small Transformers are optional CPU-bounded challengers. They must beat ridge on paired rolling out-of-sample windows after multiple-testing adjustment before becoming a champion.
- Isotonic calibration maps score percentiles to realised excess returns using a separate historical tail.
- Recency windows, half-lives, candidate features, model recipes and meta-labels are recurring trials in the SQLite ledger.

## Evaluation rules

Every split is chronological. Labels are forward returns from stored candles. A purge separates training and test periods wherever label horizons could overlap. Hyperparameters and early stopping use training-side data only. Final comparisons are paired on identical out-of-sample windows, uncertainty resamples time blocks, transaction costs are included where a prediction becomes a trade simulation, and shuffled-label controls must behave like chance.

Only deterministic stored-candle backtests and walk-forward results may update trusted memory or name a winner. Paper/live outcomes are monitoring data and cannot become training labels, select hyperparameters, or promote a model. The online language-model ensemble proposes experiments only; it does not fit numerical models or judge their success.

## Why these additions

The design follows the practical lessons in scikit-learn's time-series examples: random train/test splits are optimistic for temporally dependent data, lagged features should be evaluated with time-aware cross-validation, and predictive uncertainty can be represented with quantiles. Its calibration guidance also requires calibration data independent of model fitting.

Primary references:

- scikit-learn, “Lagged features for time series forecasting”: https://scikit-learn.org/stable/auto_examples/applications/plot_time_series_lagged_features.html
- scikit-learn, “Probability calibration”: https://scikit-learn.org/stable/modules/calibration.html
- scikit-learn, `TimeSeriesSplit`: https://scikit-learn.org/stable/modules/generated/sklearn.model_selection.TimeSeriesSplit.html
- scikit-learn, “Ensembles: Gradient boosting, random forests, bagging”: https://scikit-learn.org/stable/modules/ensemble.html

## Promotion boundary

An ML result first becomes a research candidate. It then has to survive the same fee-inclusive stored-candle walk-forward and full-validation gates as any hand-written option. Adoption into the lab model registry is not permission to trade. The order path remains deterministic, paper-only, and independently guarded.
