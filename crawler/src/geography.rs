use dg_xch_servers::discovery::{Store, sqlx};
use maxminddb::Reader;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::{io::Error, path::PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeoConfig {
    pub country_database: Option<PathBuf>,
    pub asn_database: Option<PathBuf>,
}
pub struct Geography {
    country: Option<Reader<Vec<u8>>>,
    asn: Option<Reader<Vec<u8>>>,
    pub epoch: u64,
}
impl Geography {
    pub fn open(c: &GeoConfig) -> Result<Self, Error> {
        let open = |p: &Option<PathBuf>| {
            p.as_ref()
                .map(Reader::open_readfile)
                .transpose()
                .map_err(Error::other)
        };
        let country = open(&c.country_database)?;
        let asn = open(&c.asn_database)?;
        // Distinguish country-only, ASN-only and a replacement of either database.
        let epoch = country
            .as_ref()
            .map_or(0_u64, |r| r.metadata().build_epoch)
            .wrapping_mul(31)
            .wrapping_add(asn.as_ref().map_or(0_u64, |r| r.metadata().build_epoch))
            & i64::MAX as u64;
        Ok(Self {
            country,
            asn,
            epoch,
        })
    }
    pub fn status(&self) -> Value {
        json!({"country_enabled":self.country.is_some(),"asn_enabled":self.asn.is_some(),"country_build_epoch":self.country.as_ref().map(|r|r.metadata().build_epoch),"asn_build_epoch":self.asn.as_ref().map(|r|r.metadata().build_epoch)})
    }
    pub async fn enrich(&self, store: &Store) -> Result<(), Error> {
        if self.country.is_none() && self.asn.is_none() {
            return Ok(());
        }
        let rows=sqlx::query("SELECT DISTINCT host(p.ip) AS ip FROM dg_discovery.peers p LEFT JOIN dg_discovery.geography g ON g.network=p.network AND g.ip=p.ip WHERE p.network=$1 AND p.publicly_routable AND (g.database_epoch IS NULL OR g.database_epoch<>$2) LIMIT 5000")
            .bind(&store.network).bind(self.epoch as i64).fetch_all(&store.pool).await.map_err(Error::other)?;
        let mut batch = Vec::new();
        for row in rows {
            let ip: String = row.get("ip");
            let address = ip.parse().map_err(Error::other)?;
            let lookup = |reader: &Option<Reader<Vec<u8>>>| -> Result<Option<Value>, Error> {
                reader
                    .as_ref()
                    .map(|r| {
                        r.lookup(address)
                            .and_then(|result| result.decode::<Value>())
                    })
                    .transpose()
                    .map(Option::flatten)
                    .map_err(Error::other)
            };
            let country = lookup(&self.country)?.unwrap_or(Value::Null);
            let asn = lookup(&self.asn)?.unwrap_or(Value::Null);
            batch.push(json!({"ip":ip,"country":country.pointer("/country/iso_code").and_then(Value::as_str),"asn":asn.get("autonomous_system_number").and_then(Value::as_u64),"organization":asn.get("autonomous_system_organization").and_then(Value::as_str).map(|s|s.chars().take(256).collect::<String>())}));
        }
        sqlx::query("INSERT INTO dg_discovery.geography(network,ip,country,asn,organization,database_epoch,updated_at) SELECT $1,x.ip::inet,x.country,x.asn,x.organization,$3,extract(epoch FROM clock_timestamp())::bigint FROM jsonb_to_recordset($2) AS x(ip text,country text,asn bigint,organization text) ON CONFLICT(network,ip) DO UPDATE SET country=excluded.country,asn=excluded.asn,organization=excluded.organization,database_epoch=excluded.database_epoch,updated_at=excluded.updated_at")
            .bind(&store.network).bind(json!(batch)).bind(self.epoch as i64).execute(&store.pool).await.map_err(Error::other)?;
        Ok(())
    }
}
