use crate::{config::Config, logger::Logger};
use chrono::NaiveDateTime;
use futures::stream::StreamExt;
use mongodb::{
    bson::{doc, Document},
    Collection, Database,
};
use reqwest::header;
use serde_derive::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct MetadataDoc {
    pub meta_hash: String,
    pub email: String,
    pub groups: Vec<String>,
    pub tax_state: String,
    pub salt: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SaleDoc {
    pub tx_hash: String,
    pub meta_hash: String,
    pub domain: String,
    pub price: f64,
    pub payer: String,
    pub timestamp: i64,
    pub expiry: i64,
    pub auto: bool,
    pub sponsor: Option<String>,
    pub sponsor_comm: Option<f64>,
    pub metadata: Vec<MetadataDoc>,
}

async fn process_sale(conf: &Config, logger: &Logger, sale: &SaleDoc) {
    // Extract the groups from the MetadataDoc and format them
    let groups_params: Vec<String> = sale.metadata[0]
        .groups
        .iter()
        .map(|group| format!("groups[]={}", group))
        .collect();

    // Construct the URL with parameters
    let url = format!(
        "{base_url}?email={email}&fields[name]={domain}&fields[expiry]={expiry}&{groups}",
        base_url = conf.email.base_url,
        email = &sale.metadata[0].email,
        domain = &sale.domain,
        expiry = match NaiveDateTime::from_timestamp_opt(sale.expiry, 0) {
            Some(time) => time.format("%Y-%m-%d %H:%M:%S").to_string(),
            _ => "none".to_string(),
        },
        groups = groups_params.join("&")
    );

    // Construct the Authorization header using the api_key from the config
    let auth_header = format!("Bearer {}", &conf.email.api_key);

    // Use reqwest to send a POST request
    let client = reqwest::Client::new();
    match client
        .post(&url)
        .header(header::AUTHORIZATION, auth_header)
        .send()
        .await
    {
        Ok(res) => {
            if !res.status().is_success() {
                logger.severe("Received non-success status from POST request.");
            }
        }
        Err(e) => {
            logger.severe(format!("Failed to send POST request: {}", e));
        }
    }
}

pub async fn process_data(conf: &Config, db: &Database, logger: &Logger) {
    let pipeline: Vec<Document> = vec![
        doc! {
            "$lookup": {
                "from": "metadata",
                "localField": "meta_hash",
                "foreignField": "meta_hash",
                "as": "metadata"
            }
        },
        doc! {
            "$match": {
                "metadata": { "$ne": [] }
            }
        },
        doc! {
            "$lookup": {
                "from": "processed",
                "localField": "meta_hash",
                "foreignField": "meta_hash",
                "as": "processed_doc"
            }
        },
        doc! {
            "$match": {
                "processed_doc": { "$eq": [] }
            }
        },
    ];

    let sales_collection: Collection<Document> = db.collection("sales");
    let mut cursor = sales_collection.aggregate(pipeline, None).await.unwrap();
    let mut processed = Vec::new();
    while let Some(result) = cursor.next().await {
        match result {
            Ok(document) => match mongodb::bson::from_document::<SaleDoc>(document) {
                Err(e) => {
                    logger.severe(format!("Error parsing doc: {}", e));
                }
                Ok(sales_doc) => {
                    process_sale(&conf, &logger, &sales_doc).await;
                    processed.push(sales_doc.meta_hash);
                }
            },
            Err(e) => {
                logger.severe(format!("Error while processing: {}", e));
            }
        }
    }
    if processed.is_empty() {
        return;
    }

    // Blacklist the processed documents
    let processed_collection: Collection<Document> = db.collection("processed");
    match processed_collection
        .insert_many(
            processed
                .iter()
                .map(|meta_hash| doc! { "meta_hash": meta_hash })
                .collect::<Vec<Document>>(),
            None,
        )
        .await
    {
        Err(e) => {
            logger.severe(format!(
                "Error inserting into 'processed' collection: {}",
                e
            ));
        }
        _ => {}
    }
}