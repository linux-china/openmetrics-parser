use crate::{
    internal::{
        CounterValueMarshal, LabelNames, MarshalledMetric, MarshalledMetricFamily,
        MetricFamilyMarshal, MetricMarshal, MetricProcesser, MetricValueMarshal, MetricsType,
    },
    public::*,
};
use pest::Parser;
use std::convert::TryFrom;
use std::fmt;

#[derive(Parser)]
#[grammar = "openmetrics/openmetrics.pest"]
struct OpenMetricsParser;

impl From<pest::error::Error<Rule>> for ParseError {
    fn from(err: pest::error::Error<Rule>) -> Self {
        ParseError::ParseError(err.to_string())
    }
}

impl From<MetricValueMarshal> for OpenMetricsValue {
    fn from(s: MetricValueMarshal) -> OpenMetricsValue {
        match s {
            MetricValueMarshal::Unknown(s) => OpenMetricsValue::Unknown(s.unwrap()),
            MetricValueMarshal::Untyped(s) => OpenMetricsValue::Untyped(s.unwrap()),
            MetricValueMarshal::Gauge(s) => OpenMetricsValue::Gauge(s.unwrap()),
            MetricValueMarshal::Counter(s) => OpenMetricsValue::Counter(s.into()),
            MetricValueMarshal::Histogram(s) => OpenMetricsValue::Histogram(s),
            MetricValueMarshal::StateSet(s) => OpenMetricsValue::StateSet(s.unwrap()),
            MetricValueMarshal::GaugeHistogram(s) => OpenMetricsValue::GaugeHistogram(s),
            MetricValueMarshal::Info => OpenMetricsValue::Info,
            MetricValueMarshal::Summary(s) => OpenMetricsValue::Summary(s),
        }
    }
}

impl MetricsType for OpenMetricsType {
    fn can_have_exemplar(&self, metric_name: &str) -> bool {
        match self {
            OpenMetricsType::Counter => metric_name.ends_with("_total"),
            OpenMetricsType::Histogram | OpenMetricsType::GaugeHistogram => {
                metric_name.ends_with("_bucket")
            }
            _ => false,
        }
    }

    fn get_ignored_labels(&self, metric_name: &str) -> &[&str] {
        match self {
            OpenMetricsType::Histogram | OpenMetricsType::GaugeHistogram
                if metric_name.ends_with("bucket") =>
            {
                &["le"]
            }
            _ => &[],
        }
    }

    fn get_type_value(&self) -> MetricValueMarshal {
        match self {
            OpenMetricsType::Histogram => MetricValueMarshal::Histogram(HistogramValue::default()),
            OpenMetricsType::GaugeHistogram => {
                MetricValueMarshal::GaugeHistogram(HistogramValue::default())
            }
            OpenMetricsType::Counter => MetricValueMarshal::Counter(CounterValueMarshal::default()),
            OpenMetricsType::Unknown => MetricValueMarshal::Unknown(None),
            OpenMetricsType::Gauge => MetricValueMarshal::Gauge(None),
            OpenMetricsType::StateSet => MetricValueMarshal::StateSet(None),
            OpenMetricsType::Summary => MetricValueMarshal::Summary(SummaryValue::default()),
            OpenMetricsType::Info => MetricValueMarshal::Info,
        }
    }

    fn can_have_units(&self) -> bool {
        matches!(
            self,
            OpenMetricsType::Counter | OpenMetricsType::Unknown | OpenMetricsType::Gauge
        )
    }

    fn can_have_multiple_lines(&self) -> bool {
        matches!(
            self,
            OpenMetricsType::Counter
                | OpenMetricsType::GaugeHistogram
                | OpenMetricsType::Histogram
                | OpenMetricsType::Summary
        )
    }
}

impl TryFrom<&str> for OpenMetricsType {
    type Error = ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "counter" => Ok(OpenMetricsType::Counter),
            "gauge" => Ok(OpenMetricsType::Gauge),
            "histogram" => Ok(OpenMetricsType::Histogram),
            "gaugehistogram" => Ok(OpenMetricsType::GaugeHistogram),
            "stateset" => Ok(OpenMetricsType::StateSet),
            "summary" => Ok(OpenMetricsType::Summary),
            "info" => Ok(OpenMetricsType::Info),
            "unknown" => Ok(OpenMetricsType::Unknown),
            _ => Err(ParseError::InvalidMetric(format!(
                "Invalid metric type: {}",
                value
            ))),
        }
    }
}

impl fmt::Display for OpenMetricsType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let out = match self {
            OpenMetricsType::Counter => "counter",
            OpenMetricsType::Gauge => "gauge",
            OpenMetricsType::Histogram => "histogram",
            OpenMetricsType::Summary => "summary",
            OpenMetricsType::Unknown => "unknown",
            OpenMetricsType::GaugeHistogram => "gaugehistogram",
            OpenMetricsType::StateSet => "stateset",
            OpenMetricsType::Info => "info",
        };

        f.write_str(out)
    }
}

impl Default for OpenMetricsType {
    fn default() -> Self {
        OpenMetricsType::Unknown
    }
}

impl From<MetricMarshal> for Sample<OpenMetricsValue> {
    fn from(s: MetricMarshal) -> Sample<OpenMetricsValue> {
        Sample::new(s.label_values, s.timestamp, s.value.into())
    }
}

impl MarshalledMetric<OpenMetricsType> for MetricMarshal {
    fn validate(&self, family: &MetricFamilyMarshal<OpenMetricsType>) -> Result<(), ParseError> {
        // All the labels are right
        if family.label_names.is_none() && !self.label_values.is_empty()
            || (family.label_names.as_ref().unwrap().names.len() != self.label_values.len())
        {
            return Err(ParseError::InvalidMetric(format!(
                "Metrics in family have different label sets: {:?} {:?}",
                &family.label_names, self.label_values
            )));
        }

        if family.unit.is_some() && family.metrics.is_empty() {
            return Err(ParseError::InvalidMetric(
                "Can't have metric with unit and no samples".to_owned(),
            ));
        }

        match &self.value {
            MetricValueMarshal::Histogram(histogram_value)
            | MetricValueMarshal::GaugeHistogram(histogram_value) => {
                let gauge_histogram = matches!(&self.value, MetricValueMarshal::GaugeHistogram(_));

                if histogram_value.buckets.is_empty() {
                    return Err(ParseError::InvalidMetric(
                        "Histograms must have at least one bucket".to_owned(),
                    ));
                }

                if !histogram_value
                    .buckets
                    .iter()
                    .any(|b| b.upper_bound == f64::INFINITY)
                {
                    return Err(ParseError::InvalidMetric(format!(
                        "Histograms must have a +INF bucket: {:?}",
                        histogram_value.buckets
                    )));
                }

                let buckets = &histogram_value.buckets;

                let has_negative_bucket = buckets.iter().any(|f| f.upper_bound < 0.);

                if has_negative_bucket {
                    if histogram_value.sum.is_some() && !gauge_histogram {
                        return Err(ParseError::InvalidMetric(
                            "Histograms cannot have a sum with a negative bucket".to_owned(),
                        ));
                    }
                } else if histogram_value.sum.is_some()
                    && histogram_value.sum.as_ref().unwrap().as_f64() < 0.
                {
                    return Err(ParseError::InvalidMetric(
                        "Histograms cannot have a negative sum without a negative bucket"
                            .to_owned(),
                    ));
                }

                if histogram_value.sum.is_some() && histogram_value.count.is_none() {
                    return Err(ParseError::InvalidMetric(
                        "Count must be present if sum is present".to_owned(),
                    ));
                }

                if histogram_value.sum.is_none() && histogram_value.count.is_some() {
                    return Err(ParseError::InvalidMetric(
                        "Sum must be present if count is present".to_owned(),
                    ));
                }

                let mut last = f64::NEG_INFINITY;
                for bucket in buckets {
                    if bucket.count.as_f64() < last {
                        return Err(ParseError::InvalidMetric(
                            "Histograms must be cumulative".to_owned(),
                        ));
                    }

                    last = bucket.count.as_f64();
                }
            }
            MetricValueMarshal::Counter(counter_value) => {
                if counter_value.value.is_none() {
                    return Err(ParseError::InvalidMetric(
                        "Counter is missing a _total".to_string(),
                    ));
                }
            }
            _ => {}
        }

        Ok(())
    }
}

impl MarshalledMetricFamily for MetricFamilyMarshal<OpenMetricsType> {
    type Error = ParseError;

    fn validate(&self) -> Result<(), ParseError> {
        if self.name.is_none() {
            return Err(ParseError::InvalidMetric(
                "Metric didn't have a name".to_string(),
            ));
        }

        if self.family_type == Some(OpenMetricsType::StateSet)
            && self.label_names.is_some()
            && !self
                .label_names
                .as_ref()
                .unwrap()
                .names
                .contains(self.name.as_ref().unwrap())
        {
            return Err(ParseError::InvalidMetric(
                "Stateset must not have a label with the same name as its MetricFamily".to_string(),
            ));
        }

        for metric in self.metrics.iter() {
            metric.validate(self)?;
        }

        Ok(())
    }

    fn process_new_metric(
        &mut self,
        metric_name: &str,
        metric_value: MetricNumber,
        label_names: Vec<String>,
        label_values: Vec<String>,
        timestamp: Option<Timestamp>,
        exemplar: Option<Exemplar>,
    ) -> Result<(), Self::Error> {
        let handlers = vec![
            (
                vec![OpenMetricsType::Histogram],
                vec![
                    (
                        "_bucket",
                        vec!["le"],
                        MetricProcesser::new(
                            |existing_metric: &mut MetricMarshal,
                             metric_value: MetricNumber,
                             label_names: Vec<String>,
                             label_values: Vec<String>,
                             exemplar: Option<Exemplar>,
                             _: bool| {
                                let bucket_bound: f64 = {
                                    let bound_index =
                                        label_names.iter().position(|s| s == "le").unwrap();

                                    let bound = &label_values[bound_index];
                                    match bound.parse() {
                                        Ok(f) => f,
                                        Err(_) => {
                                            return Err(ParseError::InvalidMetric(format!(
                                                "Invalid histogram bound: {}",
                                                bound
                                            )));
                                        }
                                    }
                                };

                                let bucket = HistogramBucket {
                                    count: metric_value,
                                    upper_bound: bucket_bound,
                                    exemplar,
                                };

                                if let MetricValueMarshal::Histogram(value) =
                                    &mut existing_metric.value
                                {
                                    value.buckets.push(bucket);
                                } else {
                                    unreachable!();
                                }

                                Ok(())
                            },
                        ),
                    ),
                    (
                        "_count",
                        vec![],
                        MetricProcesser::new(
                            |existing_metric: &mut MetricMarshal,
                             metric_value: MetricNumber,
                             _: Vec<String>,
                             _: Vec<String>,
                             _: Option<Exemplar>,
                             _: bool| {
                                if let MetricValueMarshal::Histogram(histogram_value) =
                                    &mut existing_metric.value
                                {
                                    let metric_value = if let Some(value) = metric_value.as_i64() {
                                        if value < 0 {
                                            return Err(ParseError::InvalidMetric(format!(
                                                "Histogram counts must be positive (got: {})",
                                                value
                                            )));
                                        }

                                        value as u64
                                    } else {
                                        return Err(ParseError::InvalidMetric(format!(
                                            "Histogram counts must be integers (got: {})",
                                            metric_value.as_f64()
                                        )));
                                    };

                                    match histogram_value.count {
                                        Some(_) => {
                                            return Err(ParseError::DuplicateMetric);
                                        }
                                        None => {
                                            histogram_value.count = Some(metric_value);
                                        }
                                    };
                                } else {
                                    unreachable!();
                                }

                                Ok(())
                            },
                        ),
                    ),
                    (
                        "_created",
                        vec![],
                        MetricProcesser::new(
                            |existing_metric: &mut MetricMarshal,
                             metric_value: MetricNumber,
                             _: Vec<String>,
                             _: Vec<String>,
                             _: Option<Exemplar>,
                             _: bool| {
                                if let MetricValueMarshal::Histogram(histogram_value) =
                                    &mut existing_metric.value
                                {
                                    match histogram_value.created {
                                        Some(_) => {
                                            return Err(ParseError::DuplicateMetric);
                                        }
                                        None => {
                                            histogram_value.created = Some(metric_value.as_f64());
                                        }
                                    };
                                } else {
                                    unreachable!();
                                }

                                Ok(())
                            },
                        ),
                    ),
                    (
                        "_sum",
                        vec![],
                        MetricProcesser::new(
                            |existing_metric: &mut MetricMarshal,
                             metric_value: MetricNumber,
                             _: Vec<String>,
                             _: Vec<String>,
                             _: Option<Exemplar>,
                             _: bool| {
                                if let MetricValueMarshal::Histogram(histogram_value) =
                                    &mut existing_metric.value
                                {
                                    if histogram_value.sum.is_some() {
                                        return Err(ParseError::DuplicateMetric);
                                    }

                                    histogram_value.sum = Some(metric_value);

                                    Ok(())
                                } else {
                                    unreachable!();
                                }
                            },
                        ),
                    ),
                ],
            ),
            (
                vec![OpenMetricsType::GaugeHistogram],
                vec![
                    (
                        "_bucket",
                        vec!["le"],
                        MetricProcesser::new(
                            |existing_metric: &mut MetricMarshal,
                             metric_value: MetricNumber,
                             label_names: Vec<String>,
                             label_values: Vec<String>,
                             exemplar: Option<Exemplar>,
                             _: bool| {
                                let bucket_bound: f64 = {
                                    let bound_index =
                                        label_names.iter().position(|s| s == "le").unwrap();

                                    let bound = &label_values[bound_index];
                                    match bound.parse() {
                                        Ok(f) => f,
                                        Err(_) => {
                                            return Err(ParseError::InvalidMetric(format!("Expected histogram bucket bound to be an f64 (got: {})", bound)));
                                        }
                                    }
                                };

                                let bucket = HistogramBucket {
                                    count: metric_value,
                                    upper_bound: bucket_bound,
                                    exemplar,
                                };

                                if let MetricValueMarshal::GaugeHistogram(value) =
                                    &mut existing_metric.value
                                {
                                    value.buckets.push(bucket);
                                } else {
                                    unreachable!();
                                }

                                Ok(())
                            },
                        ),
                    ),
                    (
                        "_gcount",
                        vec![],
                        MetricProcesser::new(
                            |existing_metric: &mut MetricMarshal,
                             metric_value: MetricNumber,
                             _: Vec<String>,
                             _: Vec<String>,
                             _: Option<Exemplar>,
                             _: bool| {
                                if let MetricValueMarshal::GaugeHistogram(histogram_value) =
                                    &mut existing_metric.value
                                {
                                    let metric_value = if let Some(value) = metric_value.as_i64() {
                                        if value < 0 {
                                            return Err(ParseError::InvalidMetric(format!(
                                                "Histogram counts must be positive (got: {})",
                                                value
                                            )));
                                        }

                                        value as u64
                                    } else {
                                        return Err(ParseError::InvalidMetric(format!(
                                            "Histogram counts must be integers (got: {})",
                                            metric_value.as_f64()
                                        )));
                                    };

                                    match histogram_value.count {
                                        Some(_) => {
                                            return Err(ParseError::DuplicateMetric);
                                        }
                                        None => {
                                            histogram_value.count = Some(metric_value);
                                        }
                                    };
                                } else {
                                    unreachable!();
                                }

                                Ok(())
                            },
                        ),
                    ),
                    (
                        "_gsum",
                        vec![],
                        MetricProcesser::new(
                            |existing_metric: &mut MetricMarshal,
                             metric_value: MetricNumber,
                             _: Vec<String>,
                             _: Vec<String>,
                             _: Option<Exemplar>,
                             _: bool| {
                                if let MetricValueMarshal::GaugeHistogram(histogram_value) =
                                    &mut existing_metric.value
                                {
                                    if histogram_value.sum.is_some() {
                                        return Err(ParseError::DuplicateMetric);
                                    }

                                    histogram_value.sum = Some(metric_value);

                                    Ok(())
                                } else {
                                    unreachable!();
                                }
                            },
                        ),
                    ),
                ],
            ),
            (
                vec![OpenMetricsType::Counter],
                vec![
                    (
                        "_total",
                        vec![],
                        MetricProcesser::new(
                            |existing_metric: &mut MetricMarshal,
                             metric_value: MetricNumber,
                             _: Vec<String>,
                             _: Vec<String>,
                             _: Option<Exemplar>,
                             _: bool| {
                                if let MetricValueMarshal::Counter(counter_value) =
                                    &mut existing_metric.value
                                {
                                    if counter_value.value.is_some() {
                                        return Err(ParseError::DuplicateMetric);
                                    }

                                    let value = metric_value.as_f64();
                                    if value < 0. || value.is_nan() {
                                        return Err(ParseError::InvalidMetric(format!(
                                            "Counter totals must be non negative (got: {})",
                                            metric_value.as_f64()
                                        )));
                                    }

                                    counter_value.value = Some(metric_value);
                                } else {
                                    unreachable!();
                                }

                                Ok(())
                            },
                        ),
                    ),
                    (
                        "_created",
                        vec![],
                        MetricProcesser::new(
                            |existing_metric: &mut MetricMarshal,
                             metric_value: MetricNumber,
                             _: Vec<String>,
                             _: Vec<String>,
                             _: Option<Exemplar>,
                             _: bool| {
                                if let MetricValueMarshal::Counter(counter_value) =
                                    &mut existing_metric.value
                                {
                                    if counter_value.created.is_some() {
                                        return Err(ParseError::DuplicateMetric);
                                    }

                                    counter_value.created = Some(metric_value.as_f64());
                                    Ok(())
                                } else {
                                    unreachable!();
                                }
                            },
                        ),
                    ),
                ],
            ),
            (
                vec![OpenMetricsType::Gauge],
                vec![(
                    "",
                    vec![],
                    MetricProcesser::new(
                        |existing_metric: &mut MetricMarshal,
                         metric_value: MetricNumber,
                         _: Vec<String>,
                         _: Vec<String>,
                         _: Option<Exemplar>,
                         _: bool| {
                            if let MetricValueMarshal::Gauge(gauge_value) =
                                &mut existing_metric.value
                            {
                                if gauge_value.is_some() {
                                    return Err(ParseError::DuplicateMetric);
                                }

                                existing_metric.value =
                                    MetricValueMarshal::Gauge(Some(metric_value));
                            } else {
                                unreachable!();
                            }

                            Ok(())
                        },
                    ),
                )],
            ),
            (
                vec![OpenMetricsType::StateSet],
                vec![(
                    "",
                    vec![],
                    MetricProcesser::new(
                        |existing_metric: &mut MetricMarshal,
                         metric_value: MetricNumber,
                         _: Vec<String>,
                         _: Vec<String>,
                         _: Option<Exemplar>,
                         _: bool| {
                            if let MetricValueMarshal::StateSet(stateset_value) =
                                &mut existing_metric.value
                            {
                                if stateset_value.is_some() {
                                    return Err(ParseError::DuplicateMetric);
                                }

                                if existing_metric.label_values.is_empty() {
                                    return Err(ParseError::InvalidMetric(
                                        "Stateset must have labels".to_string(),
                                    ));
                                }

                                if metric_value.as_f64() != 0.
                                    && (metric_value.as_f64() - 1.).abs() > f64::EPSILON
                                {
                                    return Err(ParseError::InvalidMetric(format!(
                                        "Stateset value must be 0 or 1 (got: {})",
                                        metric_value.as_f64()
                                    )));
                                }

                                existing_metric.value =
                                    MetricValueMarshal::StateSet(Some(metric_value));
                            } else {
                                unreachable!();
                            }

                            Ok(())
                        },
                    ),
                )],
            ),
            (
                vec![OpenMetricsType::Unknown],
                vec![(
                    "",
                    vec![],
                    MetricProcesser::new(
                        |existing_metric: &mut MetricMarshal,
                         metric_value: MetricNumber,
                         _: Vec<String>,
                         _: Vec<String>,
                         _: Option<Exemplar>,
                         _: bool| {
                            if let MetricValueMarshal::Unknown(unknown_value) =
                                &mut existing_metric.value
                            {
                                if unknown_value.is_some() {
                                    return Err(ParseError::DuplicateMetric);
                                }

                                existing_metric.value =
                                    MetricValueMarshal::Unknown(Some(metric_value));
                            } else {
                                unreachable!();
                            }

                            Ok(())
                        },
                    ),
                )],
            ),
            (
                vec![OpenMetricsType::Info],
                vec![(
                    "_info",
                    vec![],
                    MetricProcesser::new(
                        |_: &mut MetricMarshal,
                         metric_value: MetricNumber,
                         _: Vec<String>,
                         _: Vec<String>,
                         _: Option<Exemplar>,
                         created: bool| {
                            let metric_value = if let Some(value) = metric_value.as_i64() {
                                value as u64
                            } else {
                                return Err(ParseError::InvalidMetric(format!(
                                    "Info values must be integers (got: {})",
                                    metric_value.as_f64()
                                )));
                            };

                            if metric_value != 1 {
                                return Err(ParseError::InvalidMetric(format!(
                                    "Info values must be 1 (got: {})",
                                    metric_value
                                )));
                            }

                            if !created {
                                return Err(ParseError::DuplicateMetric);
                            }

                            Ok(())
                        },
                    ),
                )],
            ),
            (
                vec![OpenMetricsType::Summary],
                vec![
                    (
                        "_count",
                        vec![],
                        MetricProcesser::new(
                            |existing_metric: &mut MetricMarshal,
                             metric_value: MetricNumber,
                             _: Vec<String>,
                             _: Vec<String>,
                             _: Option<Exemplar>,
                             _: bool| {
                                if let MetricValueMarshal::Summary(summary_value) =
                                    &mut existing_metric.value
                                {
                                    let metric_value = if let Some(value) = metric_value.as_i64() {
                                        if value < 0 {
                                            return Err(ParseError::InvalidMetric(format!(
                                                "Summary counts must be positive (got: {})",
                                                value
                                            )));
                                        }
                                        value as u64
                                    } else {
                                        return Err(ParseError::InvalidMetric(format!(
                                            "Summary counts must be integers (got: {})",
                                            metric_value.as_f64()
                                        )));
                                    };

                                    if summary_value.count.is_none() {
                                        summary_value.count = Some(metric_value);
                                    } else {
                                        return Err(ParseError::DuplicateMetric);
                                    }
                                } else {
                                    unreachable!();
                                }

                                Ok(())
                            },
                        ),
                    ),
                    (
                        "_sum",
                        vec![],
                        MetricProcesser::new(
                            |existing_metric: &mut MetricMarshal,
                             metric_value: MetricNumber,
                             _: Vec<String>,
                             _: Vec<String>,
                             _: Option<Exemplar>,
                             _: bool| {
                                let value = metric_value.as_f64();
                                if value < 0. || value.is_nan() {
                                    return Err(ParseError::InvalidMetric(format!(
                                        "Summary sums must be non negative (got: {})",
                                        metric_value.as_f64()
                                    )));
                                }

                                if let MetricValueMarshal::Summary(summary_value) =
                                    &mut existing_metric.value
                                {
                                    if summary_value.sum.is_none() {
                                        summary_value.sum = Some(metric_value);
                                        Ok(())
                                    } else {
                                        Err(ParseError::DuplicateMetric)
                                    }
                                } else {
                                    unreachable!();
                                }
                            },
                        ),
                    ),
                    (
                        "",
                        vec!["quantile"],
                        MetricProcesser::new(
                            |existing_metric: &mut MetricMarshal,
                             metric_value: MetricNumber,
                             label_names: Vec<String>,
                             label_values: Vec<String>,
                             _: Option<Exemplar>,
                             _: bool| {
                                let value = metric_value.as_f64();
                                if !value.is_nan() && value < 0. {
                                    return Err(ParseError::InvalidMetric(
                                        "Summary quantiles can't be negative".to_owned(),
                                    ));
                                }

                                let bucket_bound: f64 = {
                                    let bound_index =
                                        label_names.iter().position(|s| s == "quantile").unwrap();
                                    let bound = &label_values[bound_index];

                                    match bound.parse() {
                                        Ok(f) => f,
                                        Err(_) => {
                                            return Err(ParseError::InvalidMetric(format!(
                                                "Summary bounds must be numbers (got: {})",
                                                bound
                                            )));
                                        }
                                    }
                                };

                                if !(0. ..=1.).contains(&bucket_bound) || bucket_bound.is_nan() {
                                    return Err(ParseError::InvalidMetric(format!(
                                        "Summary bounds must be between 0 and 1 (got: {})",
                                        bucket_bound
                                    )));
                                }

                                let quantile = Quantile {
                                    quantile: bucket_bound,
                                    value: metric_value,
                                };

                                if let MetricValueMarshal::Summary(summary_value) =
                                    &mut existing_metric.value
                                {
                                    summary_value.quantiles.push(quantile);
                                } else {
                                    unreachable!();
                                }

                                Ok(())
                            },
                        ),
                    ),
                ],
            ),
        ];

        let metric_type = self.family_type.as_ref().cloned().unwrap_or_default();

        if !metric_type.can_have_exemplar(metric_name) && exemplar.is_some() {
            return Err(ParseError::InvalidMetric(format!(
                "Metric Type {:?} is not allowed exemplars",
                metric_type
            )));
        }

        for (test_type, actions) in handlers {
            if test_type.contains(&metric_type) {
                for (suffix, mandatory_labels, action) in actions {
                    if !metric_name.ends_with(suffix) {
                        continue;
                    }

                    let mut actual_label_names = label_names.clone();
                    let mut actual_label_values = label_values.clone();
                    for label in mandatory_labels {
                        if !label_names.contains(&label.to_owned()) {
                            return Err(ParseError::InvalidMetric(format!(
                                "Missing mandatory label for metric: {}",
                                label
                            )));
                        }

                        let index = actual_label_names.iter().position(|s| s == label).unwrap();

                        actual_label_names.remove(index);
                        actual_label_values.remove(index);
                    }

                    match &self.current_label_set {
                        None => self.current_label_set = Some(actual_label_values.clone()),
                        Some(s) => {
                            if s != &actual_label_values
                                && self.seen_label_sets.contains(&actual_label_values)
                            {
                                return Err(ParseError::InvalidMetric(format!(
                                    "Interwoven labelsets: Found {:?} after {:?}",
                                    s,
                                    self.current_label_set.as_ref().unwrap()
                                )));
                            }
                        }
                    }

                    self.current_label_set = Some(actual_label_values.clone());
                    self.seen_label_sets.push(actual_label_values.clone());

                    let name = &metric_name.to_owned();
                    self.try_set_label_names(
                        name,
                        LabelNames::new(name, metric_type, actual_label_names),
                    )?;

                    let metric_name = metric_name.trim_end_matches(suffix);
                    if self.name.is_some() && self.name.as_ref().unwrap() != metric_name {
                        return Err(ParseError::InvalidMetric(format!(
                            "Invalid Name in metric family: {} != {}",
                            metric_name,
                            self.name.as_ref().unwrap()
                        )));
                    } else if self.name.is_none() {
                        self.name = Some(metric_name.to_owned());
                    }

                    let (existing_metric, created) = match self
                        .get_metric_by_labelset_mut(&actual_label_values)
                    {
                        Some(metric) => {
                            match (metric.timestamp.as_ref(), timestamp.as_ref()) {
                                (Some(metric_timestamp), Some(timestamp)) if timestamp < metric_timestamp => return Err(ParseError::InvalidMetric(format!("Timestamps went backwarts in family - saw {} and then saw{}", metric_timestamp, timestamp))),
                                (Some(_), None) | (None, Some(_)) => return Err(ParseError::InvalidMetric("Missing timestamp in family (one metric had a timestamp, another didn't)".to_string())),
                                (Some(metric_timestamp), Some(timestamp)) if timestamp >= metric_timestamp && !metric_type.can_have_multiple_lines() => return Ok(()),
                                _ => (metric, false)
                            }
                        }
                        None => {
                            let new_metric = self
                                .family_type
                                .as_ref()
                                .unwrap_or(&OpenMetricsType::Unknown)
                                .get_type_value();
                            self.add_metric(MetricMarshal::new(
                                actual_label_values.clone(),
                                timestamp,
                                new_metric,
                            ));
                            (
                                self.get_metric_by_labelset_mut(&actual_label_values)
                                    .unwrap(),
                                true,
                            )
                        }
                    };

                    return action.0(
                        existing_metric,
                        metric_value,
                        label_names,
                        label_values,
                        exemplar,
                        created,
                    );
                }
            }
        }

        return Err(ParseError::InvalidMetric(format!(
            "Found weird metric name for type ({:?}): {}",
            metric_type, metric_name
        )));
    }
}

impl From<MetricFamilyMarshal<OpenMetricsType>>
    for MetricFamily<OpenMetricsType, OpenMetricsValue>
{
    fn from(marshal: MetricFamilyMarshal<OpenMetricsType>) -> Self {
        assert!(marshal.name.is_some());

        MetricFamily::new(
            marshal.name.unwrap(),
            marshal
                .label_names
                .map(|names| names.names)
                .unwrap_or_default(),
            marshal.family_type.unwrap_or_default(),
            marshal.help.unwrap_or_default(),
            marshal.unit.unwrap_or_default(),
        )
        .with_samples(marshal.metrics.into_iter().map(|m| m.into()))
        .unwrap()
    }
}

pub fn parse_openmetrics(
    exposition_bytes: &str,
) -> Result<MetricsExposition<OpenMetricsType, OpenMetricsValue>, ParseError> {
    use pest::iterators::Pair;

    fn parse_metric_descriptor(
        pair: Pair<Rule>,
        family: &mut MetricFamilyMarshal<OpenMetricsType>,
    ) -> Result<(), ParseError> {
        assert_eq!(pair.as_rule(), Rule::metricdescriptor);

        let mut descriptor = pair.into_inner();
        let descriptor_type = descriptor.next().unwrap();
        let metric_name = descriptor.next().unwrap().as_str().to_string();

        match descriptor_type.as_rule() {
            Rule::kw_help => {
                let help_text = descriptor.next().map(|s| s.as_str()).unwrap_or_default();
                family.set_or_test_name(metric_name)?;
                family.try_add_help(help_text.to_string())?;
            }
            Rule::kw_type => {
                let family_type = descriptor.next().unwrap().as_str();
                family.set_or_test_name(metric_name)?;
                family.try_add_type(OpenMetricsType::try_from(family_type)?)?;
            }
            Rule::kw_unit => {
                let unit = descriptor.next().map(|s| s.as_str()).unwrap_or_default();
                if family.name.is_none() || &metric_name != family.name.as_ref().unwrap() {
                    return Err(ParseError::InvalidMetric(
                        "UNIT metric name doesn't match family".to_owned(),
                    ));
                }
                family.try_add_unit(unit.to_string())?;
            }
            _ => unreachable!(),
        }

        Ok(())
    }

    fn parse_exemplar(pair: Pair<Rule>) -> Result<Exemplar, ParseError> {
        let mut inner = pair.into_inner();

        let labels = inner.next().unwrap();
        assert_eq!(labels.as_rule(), Rule::labels);

        let labels = parse_labels(labels)?
            .into_iter()
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .collect();

        let id = inner.next().unwrap().as_str();
        let id = match id.parse() {
            Ok(i) => i,
            Err(_) => {
                return Err(ParseError::InvalidMetric(format!(
                    "Exemplar value must be a number (got: {})",
                    id
                )))
            }
        };

        let timestamp = match inner.next() {
            Some(timestamp) => match timestamp.as_str().parse() {
                Ok(f) => Some(f),
                Err(_) => {
                    return Err(ParseError::InvalidMetric(format!(
                        "Exemplar timestamp must be a number (got: {})",
                        timestamp.as_str()
                    )))
                }
            },
            None => None,
        };

        Ok(Exemplar::new(labels, id, timestamp))
    }

    fn parse_labels(pair: Pair<Rule>) -> Result<Vec<(&str, &str)>, ParseError> {
        assert_eq!(pair.as_rule(), Rule::labels);

        let mut label_pairs = pair.into_inner();
        let mut labels: Vec<(&str, &str)> = Vec::new();

        while label_pairs.peek().is_some() && label_pairs.peek().unwrap().as_rule() == Rule::label {
            let mut label = label_pairs.next().unwrap().into_inner();
            let name = label.next().unwrap().as_str();
            let value = label.next().unwrap().as_str();

            if labels.iter().any(|(n, _)| n == &name) {
                return Err(ParseError::InvalidMetric(format!(
                    "Found label `{}` twice in the same labelset",
                    name
                )));
            }

            labels.push((name, value));
        }

        labels.sort_by_key(|l| l.0);

        Ok(labels)
    }

    fn parse_sample(
        pair: Pair<Rule>,
        family: &mut MetricFamilyMarshal<OpenMetricsType>,
    ) -> Result<(), ParseError> {
        assert_eq!(pair.as_rule(), Rule::sample);

        let mut descriptor = pair.into_inner();
        let metric_name = descriptor.next().unwrap().as_str();

        let labels = if descriptor.peek().unwrap().as_rule() == Rule::labels {
            parse_labels(descriptor.next().unwrap())?
        } else {
            Vec::new()
        };

        let (label_names, label_values) = {
            let mut names = Vec::new();
            let mut values = Vec::new();
            for (name, value) in labels.into_iter() {
                names.push(name.to_owned());
                values.push(value.to_owned());
            }

            (names, values)
        };

        let value = descriptor.next().unwrap().as_str();
        let value = match value.parse() {
            Ok(f) => MetricNumber::Int(f),
            Err(_) => match value.parse() {
                Ok(f) => MetricNumber::Float(f),
                Err(_) => {
                    return Err(ParseError::InvalidMetric(format!(
                        "Metric Value must be a number (got: {})",
                        value
                    )));
                }
            },
        };

        let mut timestamp = None;
        let mut exemplar = None;

        if descriptor.peek().is_some()
            && descriptor.peek().as_ref().unwrap().as_rule() == Rule::timestamp
        {
            timestamp = Some(descriptor.next().unwrap().as_str().parse().unwrap());
        }

        if descriptor.peek().is_some()
            && descriptor.peek().as_ref().unwrap().as_rule() == Rule::exemplar
        {
            exemplar = Some(parse_exemplar(descriptor.next().unwrap())?);
        }

        family.process_new_metric(
            metric_name,
            value,
            label_names,
            label_values,
            timestamp,
            exemplar,
        )?;

        Ok(())
    }

    fn parse_metric_family(
        pair: Pair<Rule>,
    ) -> Result<MetricFamily<OpenMetricsType, OpenMetricsValue>, ParseError> {
        assert_eq!(pair.as_rule(), Rule::metricfamily);

        let mut metric_family = MetricFamilyMarshal::empty();

        for child in pair.into_inner() {
            match child.as_rule() {
                Rule::metricdescriptor => {
                    if metric_family.metrics.is_empty() {
                        parse_metric_descriptor(child, &mut metric_family)?;
                    } else {
                        return Err(ParseError::InvalidMetric(
                            "Metric Descriptor after samples".to_owned(),
                        ));
                    }
                }
                Rule::sample => {
                    parse_sample(child, &mut metric_family)?;
                }
                _ => unreachable!(),
            }
        }

        metric_family.validate()?;

        Ok(metric_family.into())
    }

    let exposition_marshal = OpenMetricsParser::parse(Rule::exposition, exposition_bytes)?
        .next()
        .unwrap();
    let mut exposition = MetricsExposition::new();

    assert_eq!(exposition_marshal.as_rule(), Rule::exposition);

    let mut found_eof = false;
    for span in exposition_marshal.into_inner() {
        match span.as_rule() {
            Rule::metricfamily => {
                let family = parse_metric_family(span)?;

                if exposition.families.contains_key(&family.family_name) {
                    return Err(ParseError::InvalidMetric(format!(
                        "Found a metric family called {}, after that family was finalised",
                        family.family_name
                    )));
                }

                exposition
                    .families
                    .insert(family.family_name.clone(), family);
            }
            Rule::kw_eof => {
                found_eof = true;

                if span.as_span().end() != exposition_bytes.len()
                    && !(span.as_span().end() == exposition_bytes.len() - 1
                        && exposition_bytes.ends_with('\n'))
                {
                    return Err(ParseError::InvalidMetric(
                        "Found text after the EOF token".to_string(),
                    ));
                }
            }
            _ => unreachable!(),
        }
    }

    if !found_eof {
        return Err(ParseError::InvalidMetric(
            "Didn't find an EOF token".to_string(),
        ));
    }

    Ok(exposition)
}
