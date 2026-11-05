//! Respostas curtas de confirmação/cancelamento de uma proposta pendente.
use crate::{
    Result,
    google::database,
    runtime::{Emitter, Runtime},
};
use std::sync::Arc;
use support_core::store::Operation;

const YES: [&str; 6] = [
    "sim",
    "confirmo",
    "confirmar",
    "pode salvar",
    "confirmar agendamento",
    "sim, pode salvar",
];
const NO: [&str; 6] = [
    "não",
    "nao",
    "cancelar",
    "cancele",
    "não salvar",
    "nao salvar",
];

/// Cancela a proposta pendente e publica o estado atualizado na interface.
fn cancel(runtime: &Runtime, id: &str, session: i64, emitter: &Emitter) -> Result<()> {
    database(&runtime.db)?.cancel_operation(id, session)?;
    emitter.send(
        "action.changed",
        serde_json::to_value(database(&runtime.db)?.operation(id)?)?,
    );
    Ok(())
}

pub(crate) async fn handle(
    runtime: &Arc<Runtime>,
    pending: Option<&Operation>,
    text: &str,
    emitter: &Emitter,
) -> Result<Option<String>> {
    let Some(op) = pending else {
        return Ok(None);
    };
    if op.status != "awaiting_confirmation" {
        return Ok(None);
    }
    let normalized = text
        .trim()
        .trim_matches(|c: char| c.is_ascii_punctuation())
        .to_lowercase();
    if YES.contains(&normalized.as_str()) {
        emitter.state("executing");
        let operation = runtime.confirm(&op.id, emitter.session).await?;
        emitter.send("action.changed", serde_json::to_value(&operation)?);
        return Ok(Some(
            runtime
                .integrations
                .get(&operation.integration)?
                .describe_result(&operation),
        ));
    }
    if NO.contains(&normalized.as_str()) {
        cancel(runtime, &op.id, emitter.session, emitter)?;
        return Ok(Some("A proposta foi cancelada. Nada foi executado.".into()));
    }
    cancel(runtime, &op.id, emitter.session, emitter)?;
    Ok(None)
}
