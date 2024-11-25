use anyhow::Result;
use rig::{
    completion::{Prompt, ToolDefinition},
    providers::openai::{self, GPT_4},
    tool::Tool,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt::Write as _;

const HN_API_BASE: &str = "https://hacker-news.firebaseio.com/v0";

// Struct to hold HN story metadata
#[derive(Debug, Deserialize, Serialize)]
struct Story {
    id: u32,
    title: String,
    url: Option<String>,
    text: Option<String>,
    by: String,
    score: Option<i32>,
    descendants: Option<i32>,  // number of comments
    time: i64,
    #[serde(rename = "type")]
    item_type: String,
    kids: Option<Vec<u32>>,
}

// Struct to hold HN comment with optional text
#[derive(Debug, Deserialize, Serialize)]
struct Comment {
    id: u32,
    text: Option<String>,  // Made optional to handle deleted/missing comments
    by: String,
    time: i64,
    parent: u32,
    #[serde(rename = "type")]
    item_type: String,
    kids: Option<Vec<u32>>,
}

// Tool to search HN stories
#[derive(Deserialize, Serialize)]
struct HNSearchTool;

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    story_type: Option<String>, // "top", "best", "new", "ask", "show", "job"
    max_results: Option<i32>,
}

#[derive(Debug, thiserror::Error)]
enum HNError {
    #[error("Network error while accessing HackerNews API: {0}")]
    Network(#[from] reqwest::Error),
    #[error("No matching stories found. Try broadening your search terms or searching different story types (top, new, best, etc.)")]
    NoResults,
    #[error("API error: {0}")]
    ApiError(String),
}

impl Tool for HNSearchTool {
    const NAME: &'static str = "search_hn";
    type Error = HNError;
    type Args = SearchArgs;
    type Output = Vec<(Story, Vec<Comment>)>;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "search_hn".to_string(),
            description: "Search for discussions on Hacker News".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for HN stories"
                    },
                    "story_type": {
                        "type": "string",
                        "description": "Type of stories to search (top, best, new, ask, show, job)",
                        "enum": ["top", "best", "new", "ask", "show", "job"]
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of stories to return (default: 5)"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let max_results = args.max_results.unwrap_or(5) as usize;
        let client = reqwest::Client::new();
        
        // Get stories based on type
        let stories_endpoint = match args.story_type.as_deref() {
            Some("top") | None => format!("{}/topstories.json", HN_API_BASE),
            Some("best") => format!("{}/beststories.json", HN_API_BASE),
            Some("new") => format!("{}/newstories.json", HN_API_BASE),
            Some("ask") => format!("{}/askstories.json", HN_API_BASE),
            Some("show") => format!("{}/showstories.json", HN_API_BASE),
            Some("job") => format!("{}/jobstories.json", HN_API_BASE),
            Some(_) => return Err(HNError::ApiError("Invalid story type".to_string())),
        };

        let story_ids: Vec<u32> = client.get(&stories_endpoint)
            .send()
            .await?
            .json()
            .await?;

        if story_ids.is_empty() {
            return Err(HNError::NoResults);
        }

        let mut results = Vec::new();
        let search_terms: Vec<String> = args.query
            .to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect();
        
        // Fetch stories and filter by search terms
        let mut stories_processed = 0;
        let mut stories_searched = 0;
        const MAX_STORIES_TO_SEARCH: usize = 100; // Limit how many stories we'll look through

        for &story_id in story_ids.iter() {
            if stories_searched >= MAX_STORIES_TO_SEARCH {
                break;
            }

            stories_searched += 1;
            
            let story_url = format!("{}/item/{}.json", HN_API_BASE, story_id);
            let story: Story = match client.get(&story_url)
                .send()
                .await?
                .json()
                .await {
                    Ok(story) => story,
                    Err(e) => {
                        println!("Warning: Failed to fetch story {}: {}", story_id, e);
                        continue;
                    }
                };

            // Check if story matches search terms
            let story_text = format!(
                "{} {} {}", 
                story.title.to_lowercase(),
                story.text.as_ref().map_or("", |s| s).to_lowercase(),
                story.by.to_lowercase()
            );

            let matches = search_terms.iter().any(|term| story_text.contains(term));
            
            if matches {
                let mut comments = Vec::new();
                
                // Fetch top comments if they exist
                if let Some(kids) = &story.kids {
                    for &comment_id in kids.iter().take(3) {
                        match client.get(&format!("{}/item/{}.json", HN_API_BASE, comment_id))
                            .send()
                            .await?
                            .json::<Comment>()
                            .await {
                                Ok(comment) => comments.push(comment),
                                Err(e) => println!("Warning: Failed to fetch comment {}: {}", comment_id, e),
                            }
                    }
                }

                results.push((story, comments));
                stories_processed += 1;

                if stories_processed >= max_results {
                    break;
                }
            }
        }

        if results.is_empty() {
            return Err(HNError::NoResults);
        }

        Ok(results)
    }
}

fn format_hn_results(results: &[(Story, Vec<Comment>)]) -> Result<String, anyhow::Error> {
    let mut output = String::new();
    
    writeln!(&mut output, "\n{:-^120}", " Hacker News Discussions ")?;
    writeln!(
        &mut output,
        "{:<50} | {:<15} | {:<10} | {:<20}",
        "Title", "Author", "Points", "Comments"
    )?;
    writeln!(&mut output, "{:-<120}", "")?;

    for (story, _comments) in results {
        let title = if story.title.len() > 47 {
            format!("{}...", &story.title[..47])
        } else {
            story.title.clone()
        };

        writeln!(
            &mut output,
            "{:<50} | {:<15} | {:<10} | {:<20}",
            title,
            story.by,
            story.score.unwrap_or(0),
            story.descendants.unwrap_or(0)
        )?;
    }

    writeln!(&mut output, "\n{:-^120}", " Detailed Discussion View ")?;

    for (i, (story, comments)) in results.iter().enumerate() {
        writeln!(&mut output, "\n{}. {}", i + 1, story.title)?;
        writeln!(&mut output, "By: {} | Points: {} | ID: {}", 
            story.by, 
            story.score.unwrap_or(0), 
            story.id
        )?;
        
        if let Some(url) = &story.url {
            writeln!(&mut output, "URL: {}", url)?;
        }
        if let Some(text) = &story.text {
            writeln!(&mut output, "\nText:\n{}\n", text)?;
        }

        if !comments.is_empty() {
            writeln!(&mut output, "\nTop Comments:")?;
            for (j, comment) in comments.iter().enumerate() {
                writeln!(&mut output, "\n{}.{} by {}:", i + 1, j + 1, comment.by)?;
                if let Some(text) = &comment.text {
                    writeln!(&mut output, "{}\n", text)?;
                } else {
                    writeln!(&mut output, "[Comment text not available]\n")?;
                }
            }
        }
        writeln!(&mut output, "{:-<120}", "")?;
    }

    Ok(output)
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let openai_client = openai::Client::from_env();

    let hn_agent = openai_client
        .agent(GPT_4)
        .preamble(
            "You are a helpful Hacker News discussion assistant that can search and analyze HN discussions. \
             When asked about a topic, use the search_hn tool to find relevant discussions. \
             You can search different types of stories (top, best, new, ask, show, job). \
             When searching, consider using broader search terms and specify the story type when relevant. \
             For example, for Rust programming discussions, you might search for 'rust lang programming' \
             in the 'top' stories. Return only the raw JSON response from the tool."
        )
        .tool(HNSearchTool)
        .build();

    // Example usage with more specific instructions
    let response = hn_agent
        .prompt(
            "rust lang programming"
        )
        .await?;

    // Parse and format the results
    let results: Vec<(Story, Vec<Comment>)> = serde_json::from_str(&response)?;
    match format_hn_results(&results) {
        Ok(formatted_output) => println!("{}", formatted_output),
        Err(e) => println!("Error formatting results: {}", e),
    }

    Ok(())
}