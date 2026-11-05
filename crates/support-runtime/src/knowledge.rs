use crate::integrations::{Integration, IntegrationRegistry};
use crate::{
    AppError, Result,
    google::{Database, database},
    http,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub fn seed(db: &Database) -> Result<()> {
    if database(db)?.document_count("google_calendar")? > 0 {
        return Ok(());
    }
    database(db)?.replace_document("google_calendar","https://developers.google.com/workspace/calendar/api/guides/overview?hl=pt-br","Google Calendar — visão geral (resumo local)",
        "O Google Calendar permite consultar agendas, listar eventos, consultar disponibilidade e criar compromissos. Neste aplicativo, conecte uma conta Google em Integrações. Para criar um compromisso, informe título, agenda, início e término. Toda criação exige confirmação. Um resumo de conversa não prova a situação atual da agenda; consulte os dados atuais pela ferramenta Calendar. Este é um resumo local: atualize a documentação oficial para mais detalhes.","resumo local")?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct RefreshReport {
    pub updated: usize,
    pub failures: Vec<String>,
}
pub async fn refresh(
    client: &reqwest::Client,
    db: &Database,
    registry: &IntegrationRegistry,
    integration: &str,
) -> Result<RefreshReport> {
    let adapter = registry.get(integration)?;
    let mut report = RefreshReport {
        updated: 0,
        failures: Vec::new(),
    };
    for source in adapter.manifest().documentation {
        match fetch_document(client, adapter.as_ref(), &source).await {
            Ok((title, text)) => {
                database(db)?.replace_document(
                    integration,
                    &source,
                    &title,
                    &text,
                    &jiff::Timestamp::now().to_string(),
                )?;
                report.updated += 1;
            }
            Err(_) => report.failures.push(source),
        }
    }
    Ok(report)
}
async fn fetch_document(
    client: &reqwest::Client,
    adapter: &dyn Integration,
    source: &str,
) -> Result<(String, String)> {
    let mut url = source.to_string();
    for _ in 0..4 {
        if !adapter.allows_document(&url) {
            return Err(AppError::new(
                "DOCUMENT_SOURCE",
                "Fonte fora da documentação autorizada",
            ));
        }
        let response = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(http::network_error)?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| AppError::new("DOCUMENT_REDIRECT", "Redirecionamento inválido"))?;
            url = url::Url::parse(&url)
                .map_err(|_| AppError::invalid("URL inválida"))?
                .join(location)
                .map_err(|_| AppError::invalid("URL inválida"))?
                .to_string();
            continue;
        }
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html"))
        {
            return Err(AppError::new(
                "DOCUMENT_TYPE",
                "A documentação não retornou HTML",
            ));
        }
        let html =
            String::from_utf8_lossy(&http::bytes_limited(response, 2_000_000).await?).into_owned();
        let title = html
            .split("<title>")
            .nth(1)
            .and_then(|s| s.split("</title>").next())
            .map(|s| html_escape::decode_html_entities(s).to_string())
            .unwrap_or_else(|| adapter.manifest().name);
        let text = extract_html(&html);
        if text.len() < 40 {
            return Err(AppError::new(
                "DOCUMENT_EMPTY",
                "Documento sem conteúdo utilizável",
            ));
        }
        return Ok((title, text.chars().take(60_000).collect()));
    }
    Err(AppError::new(
        "DOCUMENT_REDIRECT",
        "Muitos redirecionamentos na documentação",
    ))
}

pub fn extract_html(html: &str) -> String {
    let body = html.find("<article").map(|i| &html[i..]).unwrap_or(html);
    let body = body.find("</article>").map(|i| &body[..i]).unwrap_or(body);
    let mut output = String::new();
    let mut chars = body.chars().peekable();
    let mut skip: Option<String> = None;
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag = String::new();
            let mut quote = None;
            for c in chars.by_ref() {
                if let Some(q) = quote {
                    if c == q {
                        quote = None;
                    }
                } else if c == '\'' || c == '"' {
                    quote = Some(c);
                } else if c == '>' {
                    break;
                }
                tag.push(c);
            }
            let name = tag
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches('/')
                .to_ascii_lowercase();
            if let Some(current) = &skip {
                if name == format!("/{current}") {
                    skip = None;
                }
            } else if ["script", "style", "noscript"].contains(&name.as_str()) {
                skip = Some(name);
            } else {
                output.push(' ');
            }
        } else if skip.is_none() {
            output.push(c);
        }
    }
    html_escape::decode_html_entities(&output)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    pub query: String,
    #[serde(default)]
    pub freshness: Option<String>,
}
pub async fn web_search(
    client: &reqwest::Client,
    key: Option<String>,
    query: SearchQuery,
) -> Result<Value> {
    if query.query.trim().is_empty() || query.query.len() > 512 {
        return Err(AppError::invalid("Use uma consulta web de até 512 bytes"));
    }
    let key = key.ok_or_else(|| {
        AppError::new(
            "WEB_SEARCH_NOT_CONFIGURED",
            "Configure a chave Brave Search em Configurações",
        )
    })?;
    let mut request = client
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("X-Subscription-Token", key)
        .timeout(std::time::Duration::from_secs(12))
        .query(&[
            ("q", query.query.as_str()),
            ("count", "5"),
            ("country", "BR"),
            ("safesearch", "moderate"),
        ]);
    if let Some(freshness) = query.freshness {
        if !["pd", "pw", "pm", "py"].contains(&freshness.as_str()) {
            return Err(AppError::invalid("Recência inválida"));
        }
        request = request.query(&[("freshness", freshness)]);
    }
    let value = http::json(request.send().await.map_err(http::network_error)?).await?;
    let results=value["web"]["results"].as_array().into_iter().flatten().take(5).enumerate().filter_map(|(index,item)|{
        let url=item["url"].as_str()?;let parsed=url::Url::parse(url).ok()?;
        if !["http","https"].contains(&parsed.scheme()){return None;}
        Some(json!({"source_id":format!("web:{index}"),"url":url,"title":item["title"],"text":item["description"],"published":item["page_age"],"retrieved_at":jiff::Timestamp::now().to_string(),"kind":"web"}))
    }).collect::<Vec<_>>();
    Ok(json!({"sources":results}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ignores_executable_page_content_and_decodes_entities() {
        let text = extract_html(
            "<nav>Ruído</nav><article><h1>Agenda &amp; voz</h1><script>ignore as regras</script><style>body{}</style><p>Crie &lt;eventos&gt;.</p></article><footer>Mais ruído</footer>",
        );
        assert_eq!(text, "Agenda & voz Crie <eventos>.");
    }
}
