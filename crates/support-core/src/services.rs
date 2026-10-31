use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceManifest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub documentation: Vec<String>,
    pub tools: Vec<String>,
}

pub trait ServiceAdapter: Send + Sync {
    fn manifest(&self) -> ServiceManifest;
    fn tool_definitions(&self) -> Vec<Value>;
    fn allows_document(&self, url: &str) -> bool;
}

pub struct GoogleCalendar;
impl ServiceAdapter for GoogleCalendar {
    fn manifest(&self) -> ServiceManifest {
        ServiceManifest { id:"google_calendar".into(), name:"Google Calendar".into(),
            description:"Entenda o serviço, consulte sua agenda e crie eventos com confirmação.".into(),
            documentation: vec![
                "https://developers.google.com/workspace/calendar?hl=pt-br".into(),
                "https://developers.google.com/workspace/calendar/api/guides/overview?hl=pt-br".into(),
                "https://developers.google.com/workspace/calendar/api/guides/create-events?hl=pt-br".into(),
                "https://developers.google.com/workspace/calendar/api/v3/reference/events/list?hl=pt-br".into(),
                "https://developers.google.com/workspace/calendar/api/v3/reference/freebusy/query?hl=pt-br".into(),
            ], tools: vec!["calendar_list".into(),"calendar_events".into(),"calendar_free_busy".into(),"calendar_prepare_event".into()] }
    }
    fn allows_document(&self, url: &str) -> bool {
        url == "https://developers.google.com/workspace/calendar"
            || url.starts_with("https://developers.google.com/workspace/calendar?")
            || url.starts_with("https://developers.google.com/workspace/calendar/")
    }
    fn tool_definitions(&self) -> Vec<Value> {
        vec![
            tool(
                "calendar_list",
                "Lista as agendas da conta conectada e permissões de escrita.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            tool(
                "calendar_events",
                "Consulta eventos no intervalo RFC3339; expande recorrências. Use page_token quando houver próxima página.",
                json!({"type":"object","properties":{"calendar_id":{"type":"string"},"start":{"type":"string"},"end":{"type":"string"},"query":{"type":"string"},"page_token":{"type":"string"}},"required":["calendar_id","start","end"],"additionalProperties":false}),
            ),
            tool(
                "calendar_free_busy",
                "Consulta disponibilidade real; não conclua que a agenda está livre se houver erro por calendário.",
                json!({"type":"object","properties":{"calendar_ids":{"type":"array","items":{"type":"string"},"maxItems":10},"start":{"type":"string"},"end":{"type":"string"}},"required":["calendar_ids","start","end"],"additionalProperties":false}),
            ),
            tool(
                "calendar_prepare_event",
                "Prepara uma proposta, SEM criar evento. Exija título, agenda, início e fim/duração; esclareça ambiguidades. A confirmação será tratada exclusivamente pelo aplicativo. Datas de dia inteiro usam fim exclusivo.",
                json!({"type":"object","properties":{"calendar_id":{"type":"string"},"summary":{"type":"string"},"start":{"type":"string"},"end":{"type":"string"},"time_zone":{"type":"string"},"all_day":{"type":"boolean"},"description":{"type":"string"}},"required":["calendar_id","summary","start","end","time_zone"],"additionalProperties":false}),
            ),
        ]
    }
}

pub fn tool(name: &str, description: &str, parameters: Value) -> Value {
    json!({"type":"function","function":{"name":name,"description":description,"parameters":parameters}})
}

pub struct ServiceRegistry {
    adapters: Vec<Box<dyn ServiceAdapter>>,
}
impl Default for ServiceRegistry {
    fn default() -> Self {
        Self {
            adapters: vec![Box::new(GoogleCalendar)],
        }
    }
}
impl ServiceRegistry {
    pub fn register(&mut self, adapter: Box<dyn ServiceAdapter>) -> Result<()> {
        let id = adapter.manifest().id;
        if id.is_empty() || self.adapters.iter().any(|a| a.manifest().id == id) {
            return Err(AppError::invalid(
                "Serviço já registrado ou identificador inválido",
            ));
        }
        self.adapters.push(adapter);
        Ok(())
    }
    pub fn get(&self, id: &str) -> Result<&dyn ServiceAdapter> {
        self.adapters
            .iter()
            .find(|a| a.manifest().id == id)
            .map(|a| a.as_ref())
            .ok_or_else(|| AppError::new("SERVICE_NOT_FOUND", "Serviço não encontrado"))
    }
    pub fn list(&self) -> Vec<ServiceManifest> {
        self.adapters.iter().map(|a| a.manifest()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_cannot_call_commit_or_delete() {
        let definitions = GoogleCalendar.tool_definitions();
        assert!(!definitions.iter().any(|v| {
            ["action_confirm", "calendar_create_event", "calendar_delete"]
                .contains(&v["function"]["name"].as_str().unwrap())
        }));
        assert_eq!(definitions.len(), 4);
    }
    #[test]
    fn documentation_urls_are_scoped_to_service() {
        assert!(GoogleCalendar.allows_document(
            "https://developers.google.com/workspace/calendar/api/guides/overview?hl=pt-br"
        ));
        assert!(
            !GoogleCalendar
                .allows_document("https://developers.google.com.evil.test/workspace/calendar/")
        );
        assert!(
            !GoogleCalendar
                .allows_document("https://developers.google.com/workspace/calendar-other")
        );
    }
}
