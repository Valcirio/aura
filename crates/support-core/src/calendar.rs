use crate::{
    error::{AppError, Result},
    memory::Memory,
    store::{NewOperation, Operation},
    tools::{ActionField, Effect},
};
use jiff::{
    Timestamp,
    civil::{Date, DateTime},
    tz::TimeZone,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const GOOGLE_CALENDAR: &str = "google_calendar";
pub const CREATE_EVENT_TOOL: &str = "calendar_create_event";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventInput {
    pub calendar_id: String,
    pub summary: String,
    pub start: String,
    pub end: String,
    pub time_zone: String,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDraft {
    pub calendar_id: String,
    pub summary: String,
    pub start: String,
    pub end: String,
    pub time_zone: String,
    pub all_day: bool,
    pub description: String,
}

fn zoned(input: &str, zone: &TimeZone) -> Result<jiff::Zoned> {
    if let Ok(timestamp) = input.parse::<Timestamp>() {
        return Ok(timestamp.to_zoned(zone.clone()));
    }
    let civil = input
        .parse::<DateTime>()
        .map_err(|_| AppError::invalid("Informe uma data e um horário válidos"))?;
    zone.to_ambiguous_zoned(civil).unambiguous().map_err(|_| {
        AppError::invalid(
            "Horário inexistente ou ambíguo nesse fuso. Informe o deslocamento UTC explicitamente.",
        )
    })
}

impl EventDraft {
    pub fn validate(input: EventInput) -> Result<Self> {
        if input.calendar_id.is_empty() || input.calendar_id.len() > 1024 {
            return Err(AppError::invalid("Escolha uma agenda"));
        }
        if input.summary.trim().is_empty()
            || input.summary.len() > 500
            || input.description.len() > 8000
        {
            return Err(AppError::invalid(
                "Informe um título de até 500 bytes e descrição de até 8000 bytes",
            ));
        }
        let zone = TimeZone::get(&input.time_zone)
            .map_err(|_| AppError::invalid("Fuso horário desconhecido"))?;
        let (start, end) = if input.all_day {
            let start: Date = input
                .start
                .parse()
                .map_err(|_| AppError::invalid("Data inicial inválida"))?;
            let end: Date = input
                .end
                .parse()
                .map_err(|_| AppError::invalid("Data final inválida"))?;
            if end <= start {
                return Err(AppError::invalid(
                    "O término exclusivo deve ser posterior ao início",
                ));
            }
            (start.to_string(), end.to_string())
        } else {
            let start = zoned(&input.start, &zone)?;
            let end = zoned(&input.end, &zone)?;
            if end.timestamp() <= start.timestamp() {
                return Err(AppError::invalid("O término deve ser posterior ao início"));
            }
            (
                start.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(),
                end.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(),
            )
        };
        Ok(Self {
            calendar_id: input.calendar_id,
            summary: input.summary.trim().into(),
            start,
            end,
            time_zone: input.time_zone,
            all_day: input.all_day,
            description: input.description,
        })
    }

    pub fn api_body(&self, event_id: &str) -> Value {
        let (start, end) = if self.all_day {
            (json!({"date":self.start}), json!({"date":self.end}))
        } else {
            (
                json!({"dateTime":self.start,"timeZone":self.time_zone}),
                json!({"dateTime":self.end,"timeZone":self.time_zone}),
            )
        };
        json!({"id":event_id,"summary":self.summary,"description":self.description,"start":start,"end":end,
            "extendedProperties":{"private":{"auraOperationId":event_id}}})
    }
    pub fn interval(&self) -> Result<(String, String)> {
        if !self.all_day {
            return Ok((self.start.clone(), self.end.clone()));
        }
        let zone =
            TimeZone::get(&self.time_zone).map_err(|_| AppError::invalid("Fuso desconhecido"))?;
        let start = zoned(&format!("{}T00:00:00", self.start), &zone)?;
        let end = zoned(&format!("{}T00:00:00", self.end), &zone)?;
        Ok((
            start.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(),
            end.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(),
        ))
    }
}

impl EventDraft {
    pub fn action_fields(&self) -> Vec<ActionField> {
        vec![
            ActionField::new(
                "Agenda",
                if self.calendar_id == "primary" {
                    "Principal".to_string()
                } else {
                    self.calendar_id.clone()
                },
            ),
            ActionField::new("Início", self.start.clone()),
            ActionField::new("Término", self.end.clone()),
            ActionField::new("Fuso", self.time_zone.clone()),
        ]
    }
}

impl Memory {
    pub fn prepare_calendar_event(
        &mut self,
        session: i64,
        account: &str,
        draft: &EventDraft,
        now: i64,
    ) -> Result<Operation> {
        self.prepare_operation(
            &NewOperation {
                session,
                integration: GOOGLE_CALENDAR,
                account,
                tool: CREATE_EVENT_TOOL,
                effect: Effect::ExternalWrite,
                summary: &draft.summary,
                fields: &draft.action_fields(),
                payload: &serde_json::to_value(draft)?,
            },
            now,
        )
    }
}

pub fn validate_interval(start: &str, end: &str) -> Result<()> {
    let start: Timestamp = start
        .parse()
        .map_err(|_| AppError::invalid("Início deve incluir data, horário e deslocamento UTC"))?;
    let end: Timestamp = end
        .parse()
        .map_err(|_| AppError::invalid("Término deve incluir data, horário e deslocamento UTC"))?;
    if end <= start || end.as_second() - start.as_second() > 366 * 86400 {
        return Err(AppError::invalid(
            "Use um intervalo positivo de até 366 dias",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> EventInput {
        EventInput {
            calendar_id: "primary".into(),
            summary: "Reunião".into(),
            start: "2026-10-30T14:00:00".into(),
            end: "2026-10-30T15:00:00".into(),
            time_zone: "America/Bahia".into(),
            all_day: false,
            description: String::new(),
        }
    }
    #[test]
    fn local_time_resolves_to_bahia_offset() {
        let draft = EventDraft::validate(input()).unwrap();
        assert_eq!(draft.start, "2026-10-30T14:00:00-03:00");
        assert_eq!(
            draft.api_body("abc123")["start"]["timeZone"],
            "America/Bahia"
        );
    }
    #[test]
    fn refuses_ambiguous_and_nonexistent_dst_times() {
        for (start, end) in [
            ("2026-11-01T01:30:00", "2026-11-01T03:30:00"),
            ("2026-03-08T02:30:00", "2026-03-08T04:00:00"),
        ] {
            let mut i = input();
            i.time_zone = "America/New_York".into();
            i.start = start.into();
            i.end = end.into();
            assert!(EventDraft::validate(i).is_err());
        }
    }
    #[test]
    fn explicit_offset_disambiguates_dst() {
        let mut i = input();
        i.time_zone = "America/New_York".into();
        i.start = "2026-11-01T01:30:00-04:00".into();
        i.end = "2026-11-01T01:30:00-05:00".into();
        assert!(EventDraft::validate(i).is_ok());
    }
    #[test]
    fn all_day_keeps_exclusive_end_date() {
        let mut i = input();
        i.all_day = true;
        i.start = "2026-10-30".into();
        i.end = "2026-10-31".into();
        let d = EventDraft::validate(i).unwrap();
        assert_eq!(d.api_body("abc123")["end"], json!({"date":"2026-10-31"}));
    }
    #[test]
    fn end_must_follow_start() {
        let mut i = input();
        i.end = i.start.clone();
        assert!(EventDraft::validate(i).is_err());
    }
}
