use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use serde::Serialize;
use std::error::Error;
use std::fs::{self,File}; 
use std::path::{Path,PathBuf};
use std::io::{BufReader, BufRead, Write};
use csv::Writer;
use polars::prelude::*;
use std::collections::{HashSet,HashMap};

// pub struct LogParser{
//     indir : String,
//     outdir : String,
//     depth : u32,
//     similarity_threshold : f64,
//     max_child : u64,
//     log_format : String,
//     log_name : Option<String>,
//     log_file_path : Option<PathBuf>,
//     df_log: Option<DataFrame>, 
//     preprocess_regex : Vec<FancyRegex>,
// }

#[derive(Debug)]
struct LogCluster {
    log_template: String,
    log_id_l: HashSet<String>, // 使用HashSet来避免重复的日志ID
}

impl LogCluster {
    fn new(log_template: String, log_id_l: Option<HashSet<String>>) -> Self {
        LogCluster {
            log_template,
            log_id_l: log_id_l.unwrap_or_else(HashSet::new),
        }
    }
}

#[derive(Debug)]
struct Node {
    child_d: HashMap<String, Box<Node>>, // 使用Box<Node>来处理递归数据结构
    depth: usize,
    digit_or_token: Option<String>,
}

impl Node {
    fn new(child_d: Option<HashMap<String, Box<Node>>>, depth: usize, digit_or_token: Option<String>) -> Self {
        Node {
            child_d: child_d.unwrap_or_else(HashMap::new),
            depth,
            digit_or_token,
        }
    }
}

#[derive(Debug)]
pub struct LogParser {
    in_dir: PathBuf,       // 输入路径
    out_dir : PathBuf,
    depth: usize,        // 树的深度
    st: f64,             // 相似度阈值
    max_child: usize,    // 内部节点的最大子节点数
    log_format : String,
    log_name : Option<String>,
    log_file_path : Option<PathBuf>,
    df_log: Option<DataFrame>, 
    preprocess_regex : Vec<FancyRegex>,     // 正则表达式列表
    keep_para: bool,     // 是否保留参数
}

impl LogParser {
    pub fn new(
        indir: Option<String>,
        outdir: Option<String>,
        depth: Option<usize>,
        st: Option<f64>,
        max_child: Option<usize>,
        log_format : String,
        preprocess_regex : Vec<&'static str>,
        keep_para: Option<bool>,
    ) -> Self {
        let default_indir = "./".to_string();
        let default_outdir = "./result/".to_string();
        let default_depth = 4;
        let default_st = 0.4;
        let default_max_child = 100;
        let default_keep_para = true;

        let preprocess_regex: Vec<FancyRegex> = preprocess_regex.iter()
        .map(|pattern| FancyRegex::new(pattern).expect("Failed to compile regex pattern"))
        .collect();

        LogParser {
            in_dir: PathBuf::from(indir.unwrap_or(default_indir)),
            out_dir : PathBuf::from(outdir.unwrap_or(default_outdir)), 
            depth: depth.unwrap_or(default_depth).saturating_sub(2), // 确保不会出现负数
            st: st.unwrap_or(default_st),
            max_child: max_child.unwrap_or(default_max_child),
            log_name: None,
            log_file_path : None,
            df_log: None,
            log_format,
            preprocess_regex,
            keep_para: keep_para.unwrap_or(default_keep_para),
        }
    }
}


// // 构造函数
// pub fn new(indir: String, outdir: String, depth : u32, similarity_threshold:f64, max_child : u64,log_format: String, preprocess_regex : Vec<&'static str>) -> Self {
//     // 编译所有正则表达式模式
//     let preprocess_regex: Vec<FancyRegex> = preprocess_regex.iter()
//         .map(|pattern| FancyRegex::new(pattern).expect("Failed to compile regex pattern"))
//         .collect();
//     LogParser {
//         indir,
//         outdir,
//         depth,
//         similarity_threshold,
//         log_format,
//         preprocess_regex,
//         log_name : None,
//         log_file_path: None, // 初始化为 None
//         df_log : None,
//     }
// }

impl LogParser {
    // 解析指定日志文件（公有方法）
    pub fn parse(&mut self, log_name: String)-> std::result::Result<(), Box<dyn Error>>{
        self.log_name = Some(log_name);
        self.log_file_path = Some(self.get_logfile_path());
        self.load_data()?;

        let df = match &self.df_log{
            Some(df) => df,
            None => panic!("df_log is None"),
        };

        // 获取 "Content" 列的数据
        let series_content = df.column("Content")?.utf8()?;
        // 获取 "LineId" 列的数据（假设 LineId 是 u32 类型）
        let series_lineid = df.column("LineId")?.utf8()?;
        // 遍历每一行
        for (line_id, content) in series_lineid.into_iter().zip(series_content.into_iter()) {
            println!("line_id:{:?}, content:{:?}",line_id,content);
            if let (Some(id), Some(content)) = (line_id, content) {
                // 先将预处理结果存储在一个持久的变量中
                let processed_content = self.preprocess(content);
                // 然后对持久变量进行分割
                let logmessageL: Vec<String> = processed_content.split_whitespace()
                    .map(|s| s.to_string())
                    .collect();
                
                println!("LogID: {}, LogMessage: {:?}", id, logmessageL);
            }
        }
        
        Ok(())

    }

    // 优化过的
    fn get_logfile_name_without_suffix(&self) -> String {        
        let log_file_name = self.get_logfile_name_with_suffix();
        let path = Path::new(&log_file_name);
        match path.file_stem().and_then(|s| s.to_str()) {
            Some(stem) => stem.to_string(),
            None => log_file_name.to_string(),
        }
    }

    fn get_logfile_name_with_suffix(&self) -> &str{
        let log_name = match &self.log_name{
            Some(name) => name,
            None => panic!("logfile_name is NULL!"),
        };
        &log_name
    }

    // 获得日志文件的地址（私有方法）
    fn get_logfile_path(&self) -> PathBuf{
        // 使用 PathBuf 来构建完整路径
        let log_name = self.get_logfile_name_with_suffix();
        let mut logfile_path = PathBuf::from(&self.in_dir);
        logfile_path.push(log_name);
        logfile_path
    }

    // 提取变量名（私有方法）
    fn extract_variable_names(&self, log_format: &str) -> Vec<String> {
        let mut headers = Vec::new();
        let re = Regex::new(r"(<[^<>]+>)").unwrap();

        for cap in re.captures_iter(log_format) {
            if let Some(header) = cap.get(1) {
                headers.push(header.as_str().to_string());
            } 
        }

        print!("{:?}", headers);
        headers
    }

    // 生成日志格式的正则表达式（私有方法）
    fn generate_logformat_regex(&self, log_format: &str) -> (Vec<String>, Regex) {
        let mut regex_pattern = String::new();
        let headers = self.extract_variable_names(log_format);
        let mut parts = Vec::new();
        let re = Regex::new(r"(<[^<>]+>|\s+|[^\s<>]+)").unwrap();

        for cap in re.captures_iter(log_format) {
            if let Some(header) = cap.get(1) {
                parts.push(header.as_str().to_string());
            } else if let Some(_) = cap.get(0) {
                parts.push(" ".to_string());
            }
        }

        for part in &parts { 
            if headers.contains(part) {
                regex_pattern.push_str(&format!(r"(?P<{}>.*?)", part.trim_matches('<').trim_matches('>')));
            } else {
                let splitter = part.replace(" +", r"\\s+");
                regex_pattern.push_str(&splitter);
            }
        }

        let final_pattern = format!(r"^{}$", regex_pattern);
        let regex = Regex::new(&final_pattern).unwrap();

        (headers, regex)
    }

    fn log_to_dataframe(&mut self, regex: &Regex, headers: &[String]) -> std::result::Result<(), Box<dyn Error>> {     
        let path = match &self.log_file_path {
            Some(path) => path,
            None => panic!("No log file path set"),
        };
        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut log_entries: Vec<HashMap<String, String>> = Vec::new();
        let mut linecount = 0;

        for line in reader.lines() {
            let line = line?;
            if let Some(caps) = regex.captures(&line.trim()) {
                let mut entry = HashMap::new();
                entry.insert("LineId".to_string(), (linecount + 1).to_string());
                for header in headers.iter() {
                    let value = caps.name(header.trim_matches('<').trim_matches('>')).map_or("", |m| m.as_str()).to_string();
                    entry.insert(header.clone(), value);
                }
                log_entries.push(entry);
                linecount += 1;
            } else {
                eprintln!("[Warning] Skip line: {}", line);
            }
        }

        // 创建 DataFrame
        let mut columns: Vec<Series> = Vec::new();
        let mut series_map: std::collections::HashMap<&String, Vec<String>> = std::collections::HashMap::new();

        for entry in &log_entries {
            for (key, value) in entry {
                series_map.entry(key).or_insert_with(Vec::new).push(value.clone());
            }
        }

        for (key, values) in series_map {
            println!("key:{:?}",key);
            columns.push(Series::new(key.trim_matches('<').trim_matches('>'), values));
        }

        let df = DataFrame::new(columns)?;

        // 存储 DataFrame 到 df_log 字段
        self.df_log = Some(df);

        println!("{:?}", &self.df_log);
        println!("Total lines: {}", log_entries.len());

        Ok(())
    }

    // 加载数据（私有方法）
    fn load_data(&mut self) -> std::result::Result<(), Box<dyn Error>> {
        let (headers, log_format_regex) = self.generate_logformat_regex(&self.log_format);

        self.log_to_dataframe(&log_format_regex, &headers)
    }
    
    // preprocess 方法，用于处理输入行
    fn preprocess(&self, line: &str) -> String {
        let mut result = line.to_string();
        for current_rex in &self.preprocess_regex {
            // replace_all 返回 Cow<str>，可以直接转换为 String
            let replacement = current_rex.replace_all(result.as_str(), "<*>").to_string();
            result = replacement;
        }
        result
    }

    // pub fn fast_match(&self){

    // }

    // pub fn tree_search(&self, rn: &Node, seq: Vec<String>) -> Option<LogCluster> {
    //     let mut ret_log_clust = None;
    //     let seq_len = seq.len();
        
    //     if !rn.child_d.contains_key(&seq_len.to_string()) {
    //         return ret_log_clust;
    //     }
        
    //     let mut parentn = rn.child_d.get(&seq_len.to_string()).unwrap();
    //     let mut current_depth = 1;
        
    //     for token in seq.iter() {
    //         if current_depth >= self.depth || current_depth > seq_len {
    //             break;
    //         }
            
    //         if let Some(next_node) = parentn.child_d.get(token) {
    //             parentn = next_node;
    //         } else if let Some(wildcard_node) = parentn.child_d.get("<*>") {
    //             parentn = wildcard_node;
    //         } else {
    //             return ret_log_clust;
    //         }
    //         current_depth += 1;
    //     }
        
    //     if let Some(log_clust_l) = parentn.child_d.get("logClustL") {
    //         ret_log_clust = self.fast_match(log_clust_l, &seq);
    //     }
        
    //     ret_log_clust
    // }
}