use std::{collections::HashMap, path::Path, str::FromStr, thread, vec};

use chrono::{DateTime, Local, NaiveDateTime};
use config::{Config, File, FileStoredFormat, Format};
use google_sheets4::{
    api::{
        AutoResizeDimensionsRequest, BatchUpdateSpreadsheetRequest, DataSourceColumnReference,
        DataSourceSheetDimensionRange, DimensionProperties, DimensionRange, GridProperties,
        Request, SheetProperties, UpdateDimensionPropertiesRequest, UpdateSheetPropertiesRequest,
        ValueRange,
    },
    hyper::{self, client::HttpConnector},
    hyper_rustls::{self, HttpsConnector},
    oauth2::{self, authenticator::Authenticator},
    Error, FieldMask, Sheets,
};
use regex::Regex;
use scraper::{Html, Selector};

use serde_json::json;
use simple_error::SimpleError;
use tokio::time;

#[derive(Debug)]
struct Submission {
    // user: String,
    score: i16,
    problem_name: String,
    date: DateTime<Local>,
}

#[derive(Debug)]
struct User {
    name: String,
    infoarena_username: String,
}

#[derive(serde::Deserialize, Clone, Debug)]
struct APIConfig {
    sheet_id: String,
    google_priv_key_path: String,
}

async fn get_monitor(
    user: &str,
    from: i16,
    count: i16,
) -> Result<String, Box<dyn std::error::Error>> {
    let url = format!(
        "https://www.infoarena.ro/monitor?user={}&display_entries={}&first_entry={}",
        user, count, from
    );
    let a = reqwest::get(&url).await;
    let ans = reqwest::get(url).await?.text().await?;

    return Ok(ans);
}

fn get_score(result_text: &str, re: &Regex) -> i16 {
    match re.captures_iter(result_text).next() {
        None => 0,
        Some(capture) => capture
            .extract::<1>()
            .1
            .first()
            .unwrap()
            .parse::<i16>()
            .unwrap(),
    }
}

fn get_datetime(date_text: &str) -> Result<DateTime<Local>, Box<dyn std::error::Error>> {
    let mut eng_date_text = date_text.to_string();
    eng_date_text = eng_date_text.replace("ian", "jan");
    eng_date_text = eng_date_text.replace("iul", "jul");
    eng_date_text = eng_date_text.replace("mai", "may");
    eng_date_text = eng_date_text.replace("iun", "jun");
    Ok(
        NaiveDateTime::parse_from_str(eng_date_text.as_str(), "%Y %b %d %H:%M:%S")
            .map_err(|err| SimpleError::new(format!("Failed to parse datetime reason: {:?}", err)))?
            .and_local_timezone(Local::now().timezone())
            .unwrap(),
    )
}

/// Return the submissions sorted in descending order of submission date
async fn parse_monitor_page(user: &str) -> Result<Vec<Submission>, Box<dyn std::error::Error>> {
    thread::sleep(time::Duration::from_millis(200));
    let body = get_monitor(user, 0, 1000).await?;

    let fragment = Html::parse_fragment(&body);
    let monitor_table_selector = Selector::parse(r#"table[class="monitor"]"#).unwrap();
    let tbody_selector = Selector::parse("tbody").unwrap();
    let tr_selector = Selector::parse("tr").unwrap();
    let td_selector = Selector::parse("td").unwrap();
    let a_selector = Selector::parse("a").unwrap();

    let monitor = fragment
        .select(&monitor_table_selector)
        .next()
        .expect("Failed to found submissions table in monitor page.");
    let monitor_body = monitor
        .select(&tbody_selector)
        .next()
        .expect("Failed to found body of the submissions table.");
    let monitor_rows = monitor_body.select(&tr_selector);

    let result_re =
        Regex::new(r"^Evaluare completa: ([0-9]+) puncte$").expect("Failed to parse result regex.");

    let mut submissions = Vec::<Submission>::new();

    for row in monitor_rows {
        let mut columns = row.select(&td_selector);

        let _id_column = columns.next().unwrap();
        let _user_column = columns.next().unwrap();
        let problem_column = columns.next().unwrap();
        let _round_column = columns.next().unwrap();
        let _size_column = columns.next().unwrap();
        let date_column = columns.next().unwrap();
        let result_column = columns.next().unwrap();

        // Get problem name
        let problem_name = problem_column
            .select(&a_selector)
            .next()
            .unwrap()
            .attr("href")
            .unwrap()
            .replace("/problema/", "");

        // Get result
        let result_text = result_column.text().next().unwrap();
        let score = get_score(result_text, &result_re);

        // Get datetime
        let date_text = date_column.text().next().unwrap();
        let datetime = get_datetime(date_text)?;

        submissions.push(Submission {
            problem_name,
            score,
            date: datetime,
        })
    }

    Ok(submissions)
}

async fn get_submissions(
    user: &str,
) -> Result<HashMap<String, Submission>, Box<dyn std::error::Error>> {
    let submissions = parse_monitor_page(user).await?;

    // 2. To do sort submissions and keep only the best submission

    let mut best_submissions = HashMap::<String, Submission>::new();
    for submission in submissions {
        match best_submissions.get(&submission.problem_name) {
            None => {
                best_submissions.insert(submission.problem_name.to_owned(), submission);
            }
            Some(old_submission) => {
                if old_submission.score < submission.score {
                    best_submissions.insert(submission.problem_name.to_owned(), submission);
                }
            }
        }
    }

    Ok(best_submissions)
}

pub fn http_client() -> hyper::Client<hyper_rustls::HttpsConnector<hyper::client::HttpConnector>> {
    return hyper::Client::builder().build(
        hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_only()
            .enable_http1()
            .enable_http2()
            .build(),
    );
}

pub async fn auth(
    client: hyper::Client<hyper_rustls::HttpsConnector<hyper::client::HttpConnector>>,
    priv_key_path: &str,
) -> Authenticator<hyper_rustls::HttpsConnector<hyper::client::HttpConnector>> {
    let secret: oauth2::ServiceAccountKey = oauth2::read_service_account_key(priv_key_path)
        .await
        .expect("secret not found");

    return oauth2::ServiceAccountAuthenticator::with_client(secret, client.clone())
        .build()
        .await
        .expect("could not create an authenticator");
}

pub async fn read(
    hub: &Sheets<hyper_rustls::HttpsConnector<hyper::client::HttpConnector>>,
    range: &str,
    sheet_id: &str,
) -> Result<(hyper::Response<hyper::Body>, ValueRange), Error> {
    return hub.spreadsheets().values_get(sheet_id, range).doit().await;
}

fn get_auto_resize_req(users_number: i32) -> Request {
    let mut auto_resize_request = Request::default();

    auto_resize_request.auto_resize_dimensions = Some(AutoResizeDimensionsRequest {
        data_source_sheet_dimensions: None,
        dimensions: Some(DimensionRange {
            dimension: Some("COLUMNS".to_string()),
            start_index: Some(0),
            end_index: Some(users_number + 1),
            sheet_id: Some(0),
        }),
    });

    auto_resize_request
}

fn get_update_sheet_proprieties_req(users_number: i32, problems_number: i32) -> Request {
    let mut req = Request::default();

    let mut update_sheet_proprieties_req = UpdateSheetPropertiesRequest::default();

    let mut sheet_proprieties = SheetProperties::default();
    let mut grid_properties = GridProperties::default();

    grid_properties.frozen_row_count = Some(1);
    // One extra column for problems column
    grid_properties.column_count = Some(users_number + 1);
    // Two extra rows for headers and footer
    grid_properties.row_count = Some(problems_number + 2);

    sheet_proprieties.sheet_id = Some(0);
    sheet_proprieties.grid_properties = Some(grid_properties);

    update_sheet_proprieties_req.properties = Some(sheet_proprieties);
    update_sheet_proprieties_req.fields = Some(
        FieldMask::from_str(
            "gridProperties.frozenRowCount, gridProperties.column_count, gridProperties.row_count",
        )
        .expect("Failed to get field mask."),
    );

    req.update_sheet_properties = Some(update_sheet_proprieties_req);

    req
}

async fn write_results(
    hub: &Sheets<HttpsConnector<HttpConnector>>,
    sheet_id: &str,
    users: Vec<User>,
    problems: Vec<String>,
    submissions: HashMap<String, HashMap<String, Submission>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let last_column_letter = ('A' as u8 + users.len() as u8) as char;
    let range = format!("Results!A1:{}{}", last_column_letter, problems.len() + 2);

    let mut header = vec![json!("Problema")];
    for user in &users {
        header.push(json!(user.name.to_owned()));
    }

    let mut rows = vec![header];
    for problem in &problems {
        let mut row = vec![json!(problem)];
        for user in &users {
            // Here we can reverse the maps order to optimize the calls...
            match submissions.get(&user.name) {
                Some(user_submissions) => match user_submissions.get(problem) {
                    Some(submission) => row.push(json!(submission.score)),
                    None => row.push(json!("")),
                },
                None => {
                    // User not found, this shouldn't happen.
                    row.push(json!(""))
                }
            }
        }
        rows.push(row);
    }

    let mut footer = vec![json!("Total")];
    let mut column_letter: char = 'B';
    for _user in &users {
        // Sum all results from the row of the first problem A2 to the last row A5 (example for 4 problems)
        footer.push(json!(format!(
            "=SUM({}2:{}{})",
            column_letter,
            column_letter,
            problems.len() + 1
        )));
        column_letter = (column_letter as u8 + 1) as char;
    }
    rows.push(footer);

    let request = ValueRange {
        major_dimension: Some("ROWS".to_string()),
        range: Some(range.to_owned()),
        values: Some(rows),
    };

    hub.spreadsheets()
        .values_update(request, sheet_id, &range)
        .value_input_option("USER_ENTERED")
        .doit()
        .await?;

    let auto_resize_req = get_auto_resize_req(users.len() as i32);
    let update_sheet_proprieties_req =
        get_update_sheet_proprieties_req(users.len() as i32, problems.len() as i32);

    let batch_update_req = BatchUpdateSpreadsheetRequest {
        include_spreadsheet_in_response: None,
        requests: Some(vec![auto_resize_req, update_sheet_proprieties_req]),
        response_include_grid_data: None,
        response_ranges: None,
    };

    hub.spreadsheets()
        .batch_update(batch_update_req, sheet_id)
        .doit()
        .await?;

    Ok(())
}

async fn get_sheets_hub(priv_key_path: &str) -> Sheets<HttpsConnector<HttpConnector>> {
    let client = http_client();
    let auth = auth(client.clone(), priv_key_path).await;
    let hub = Sheets::new(client.clone(), auth);

    return hub;
}

async fn get_users(
    hub: &Sheets<HttpsConnector<HttpConnector>>,
    sheet_id: &str,
) -> Result<Vec<User>, Box<dyn std::error::Error>> {
    let mut users = Vec::<User>::new();

    // We have the list of names on column A and list of infoarena usernames on column B. We read at most 19 names.
    let (_, result) = read(hub, "Input!A2:B20", sheet_id).await?;

    for row in result.values.expect("No usernames found.") {
        match &row[..] {
            [name, username] => users.push(User {
                name: name.as_str().unwrap_or_default().to_string(),
                infoarena_username: username.as_str().unwrap_or_default().to_string(),
            }),
            _ => (),
        };
    }

    Ok(users)
}

async fn get_problem_list(
    hub: &Sheets<HttpsConnector<HttpConnector>>,
    sheet_id: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let (_, result) = read(hub, "Input!D2:D1000", sheet_id).await?;
    let mut problems = Vec::<String>::new();

    for row in result.values.expect("No problems found.") {
        match &row[..] {
            [problem_name] => problems.push(problem_name.as_str().unwrap_or_default().to_string()),
            _ => (),
        };
    }

    Ok(problems)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_file = File::from(Path::new("config.yaml"));
    let config = Config::builder()
        .add_source(config_file)
        .build()
        .expect("Failed to load config file.");
    let config: APIConfig = config
        .try_deserialize()
        .expect("Failed to deserialize config file.");

    let priv_key_path = config.google_priv_key_path.as_str();
    let sheet_id = config.sheet_id.as_str();

    let hub = get_sheets_hub(priv_key_path).await;

    let users: Vec<User> = get_users(&hub, sheet_id).await?;
    let problems = get_problem_list(&hub, sheet_id).await?;

    println!("Result {:?}", users);
    println!("Problems {:?}", problems);

    let mut submissions = HashMap::<String, HashMap<String, Submission>>::new();

    for user in &users {
        let user_submissions = get_submissions(&user.infoarena_username).await?;
        submissions.insert(user.name.to_owned(), user_submissions);
    }

    println!("Submissions: {:?}", submissions);

    write_results(&hub, sheet_id, users, problems, submissions).await?;

    Ok(())
}
