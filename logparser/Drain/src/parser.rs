use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use serde::Serialize;
use std::cell::RefCell;
use std::error::Error;
use std::fs::{self,File}; 
use std::path::{Path,PathBuf};
use std::io::{BufReader, BufRead, Write};
use std::rc::Rc;
use csv::Writer;
use polars::prelude::*;
use std::collections::{HashSet,HashMap};
use md5::{Digest, Md5};

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

// * 日志组
#[derive(Debug,Clone)]
struct LogCluster{
    log_id_lists : Vec<String>,
    log_event: Vec<String>,
}

impl LogCluster{
    fn new(log_id_lists : Vec<String>,log_event: Vec<String>) -> Self {
        LogCluster{
            log_id_lists,
            log_event,
        }
    }
}

#[derive(Debug)]
enum ChildOrLogCluster {
    Children(HashMap<String, Rc<RefCell<Node>>>),
    LogClusters(Vec<LogCluster>),
}

#[derive(Debug)]
struct Node {
    depth: usize,
    digit_or_token: String,
    child_or_logcluster : ChildOrLogCluster,
}

impl Node {
    fn new(depth: Option<usize>, digit_or_token: Option<String>) -> Self {
        let depth = match depth{
            Some(dep) => dep,
            None => 0,
        };
        let digit_or_token = match digit_or_token{
            Some(digit_or_token) => digit_or_token,
            None => String::new(),
        };
        Node {
            depth,
            digit_or_token,
            child_or_logcluster: ChildOrLogCluster::Children(HashMap::new()),
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

        let mut count = 0;
        let mut root_node = Node::new(None,None);
        let mut log_clust:Vec<LogCluster> = Vec::new();
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
            //*test */
            // println!("line_id:{:?}, content:{:?}",line_id,content);
            if let (Some(id), Some(content)) = (line_id, content) {
                // 先将预处理结果存储在一个持久的变量中
                let processed_content = self.preprocess(content);
                // 然后对持久变量进行分割
                let logmessage_list: Vec<String> = processed_content.split_whitespace()
                    .map(|s| s.to_string())
                    .collect();
                
                //*test */
                // println!("LogID: {}, LogMessage: {:?}", id, &logmessage_list);
                let match_cluster = self.tree_search(&mut root_node, &logmessage_list);

                //*test */
                // println!("{:?}", match_cluster);
                if let Some(mut match_cluster) = match_cluster{
                    let new_template = self.get_template(&logmessage_list,&match_cluster.log_event);
                    match_cluster.log_id_lists.push(id.to_string());

                    // 检查新的模板是否与现有的不同
                    if new_template.join(" ") != match_cluster.log_event.join(" ") {
                        match_cluster.log_event = new_template;
                    }
                    
                }else{
                    let new_logcluster = LogCluster::new(vec![id.to_string()], logmessage_list);                  
                    self.add_seq_to_prefix_tree(&mut root_node, &new_logcluster);
                    log_clust.push(new_logcluster);
                }

                count += 1;

                if let Some(df) = &self.df_log {
                    let total_rows = df.height();
                    if count % 1000 == 0 || count == total_rows{
                        let progress:f64;
                        if count == 0{
                            progress = 0.0;
                        }
                        else{
                            progress = (count as f64) * 100.0 / (total_rows as f64);
                        }
                        println!("Processed {:.1}%", progress);
                    }
                }
            }
        }

        if !Path::new(&self.out_dir).exists() {
            fs::create_dir_all(&self.out_dir).expect("Failed to create directory");
        }

        self.output_result(log_clust);

        self.print_tree(&root_node, 1);
        
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

    fn has_numbers(&self, token: &str) -> bool {
        token.chars().any(|c| c.is_digit(10))
    }

    fn add_seq_to_prefix_tree(
        &self,
        root_node: &mut Node,
        log_clust: &LogCluster,
    ) {
        let seq_len = log_clust.log_event.len();
        let seq_len_str = seq_len.to_string();
        let first_layer_node = match &mut root_node.child_or_logcluster {
            ChildOrLogCluster::Children(children_map) => {
                if children_map.contains_key(&seq_len_str) {
                    // 如果存在，则获取并返回对应的节点
                    Rc::clone(children_map.get(&seq_len_str).unwrap())
                } else {
                    // 如果不存在，则创建新节点并插入到 map 中
                    let new_node = Rc::new(RefCell::new(Node::new(Some(1), Some(seq_len_str.clone()))));
                    children_map.insert(seq_len_str.clone(), Rc::clone(&new_node));
                    Rc::clone(&new_node)
                }
            },
            _ => panic!("first_layer_node is logclust"),
        };

        let mut parent_node = Rc::clone(&first_layer_node);

        let mut currrent_depth = 1;

        for token in &log_clust.log_event{
            if currrent_depth >= self.depth || currrent_depth > seq_len{
                let mut parent_node_borrow = parent_node.borrow_mut();
                match &mut parent_node_borrow.child_or_logcluster{
                    ChildOrLogCluster::LogClusters(log_clusts) =>{
                        log_clusts.push(log_clust.clone());
                    }
                    ChildOrLogCluster::Children(_)=>{
                        parent_node_borrow.child_or_logcluster = ChildOrLogCluster::LogClusters(vec![log_clust.clone()]);
                    }
                }

                break;
            }

            let mut parent_borrow = parent_node.borrow_mut();
            match &mut parent_borrow.child_or_logcluster {
                ChildOrLogCluster::Children(children_map) => {
                    if let Some(next_node) = children_map.get(token) {
                        // 使用局部变量存储新节点引用
                        let next_node_clone = Rc::clone(next_node);
                        drop(parent_borrow); // 释放当前借用
                        parent_node = next_node_clone;
                    } 
                    // token not in parent_node.children
                    else {
                        if !self.has_numbers(token){
                            if let Some(next_node) = children_map.get("<*>") {
                                if children_map.len() < self.max_child{
                                    let new_node = Node::new(Some(currrent_depth + 1), Some(token.clone()));
                                    let new_node = Rc::new(RefCell::new(new_node));
                                    children_map.insert(token.clone(), Rc::clone(&new_node));
                                    drop(parent_borrow);
                                    parent_node = Rc::clone(&new_node);
                                }else{
                                    // 使用局部变量存储新节点引用
                                    let next_node_clone = Rc::clone(next_node);
                                    drop(parent_borrow); // 释放当前借用
                                    parent_node = next_node_clone;
                                }
                            } 
                            else{
                                if children_map.len() + 1 < self.max_child{
                                    let new_node = Node::new(Some(currrent_depth + 1),Some(token.clone()));
                                    let new_node = Rc::new(RefCell::new(new_node));
                                    children_map.insert(token.clone(), Rc::clone(&new_node));
                                    drop(parent_borrow);
                                    parent_node = Rc::clone(&new_node);
                                }else if children_map.len() + 1 == self.max_child {
                                    let new_node = Node::new(Some(currrent_depth + 1),Some( "<*>".to_string()));
                                    let new_node = Rc::new(RefCell::new(new_node));
                                    children_map.insert("<*>".to_string(), Rc::clone(&new_node));
                                    drop(parent_borrow);
                                    parent_node = Rc::clone(&new_node);
                                }else{
                                    // !有待考证
                                    unreachable!()
                                }
                            }
                        }
                        // token has numbers
                        else{
                            // child has "<*>"
                            if let Some(next_node) = children_map.get("<*>") {
                                // 使用局部变量存储新节点引用
                                let next_node_clone = Rc::clone(next_node);
                                drop(parent_borrow); // 释放当前借用
                                parent_node = next_node_clone;
                            } 
                            else{
                                let new_node = Node::new(Some(currrent_depth + 1),Some( "<*>".to_string()));
                                let new_node = Rc::new(RefCell::new(new_node));
                                children_map.insert("<*>".to_string(), Rc::clone(&new_node));
                                drop(parent_borrow);
                                parent_node = Rc::clone(&new_node);
                            }
                        }
                    }
                }
                _ => panic!("Unexpected child_or_logcluster variant"),
            }
            currrent_depth += 1;
        }
    }

    fn seq_dist(&self,seq1: &Vec<String>, seq2: &Vec<String>) -> (f64, i32) {
        // 确保两个序列长度相同
        assert_eq!(seq1.len(), seq2.len(), "The sequences must have the same length.");
    
        let mut sim_tokens = 0;
        let mut num_of_par = 0;
    
        for (token1, token2) in seq1.iter().zip(seq2.iter()) {
            if token1 == "<*>" {
                num_of_par += 1;
                continue;
            }
            if token1 == token2 {
                sim_tokens += 1;
            }
        }
    
        // 计算相似令牌的比例
        let ret_val = sim_tokens as f64 / seq1.len() as f64;
    
        (ret_val, num_of_par)
    }

    fn get_template(&self,seq1: &[String], seq2: &[String]) -> Vec<String> {
        // 确保两个序列长度相同
        assert_eq!(seq1.len(), seq2.len(), "Sequences must be of the same length");
    
        let mut ret_val = Vec::with_capacity(seq1.len());
    
        for (word1, word2) in seq1.iter().zip(seq2.iter()) {
            if word1 == word2 {
                ret_val.push(word1.clone());
            } else {
                ret_val.push("<*>".to_string());
            }
        }
    
        ret_val
    }

    fn fast_match<'a>(&self, log_cluster_l:&'a Vec<LogCluster>, seq: &Vec<String>)->Option<&'a LogCluster>{
        let mut ret_log_clust:Option<&LogCluster> = None;

        let mut max_sim = -1.0;
        let mut max_num_of_para = -1;
        let mut max_clust:Option<&LogCluster> = None;

        for log_clust in log_cluster_l{
           let (cur_sim, cur_num_of_para) = self.seq_dist(&log_clust.log_event, seq);
           if cur_sim > max_sim || (cur_sim == max_sim && cur_num_of_para > max_num_of_para){
                max_sim = cur_sim;
                max_num_of_para = cur_num_of_para;
                max_clust = Some(log_clust);
           } 
        }

        if max_sim >= self.st{
            ret_log_clust = max_clust;
        }

        ret_log_clust
    }

    fn tree_search(&self, root_node: &mut Node, seq: &Vec<String>)->Option<LogCluster>{
        let seq_len = seq.len();
        let seq_len_str = seq.len().to_string();

        let mut parent_node:Rc<RefCell<Node>>;

        match &root_node.child_or_logcluster{
            ChildOrLogCluster::Children(children_map)=>{
                if !children_map.contains_key(&seq_len_str){
                    return None;
                }
                parent_node = Rc::clone(children_map.get(&seq_len_str).unwrap());
            }
            _ => panic!("first_layer_node is logcluster")
        }

        let mut current_depth = 1;

        for token in seq{
            let mut parent_borrow = parent_node.borrow_mut();
            if current_depth >= self.depth || current_depth > seq_len{
                break;
            }
            match &mut parent_borrow.child_or_logcluster{
                ChildOrLogCluster::Children(children_map)=>{
                    if children_map.contains_key(token){
                        let next_node = children_map.get(token).unwrap();
                        let next_node_clone = Rc::clone(next_node);
                        drop(parent_borrow);
                        parent_node = next_node_clone;
                    }else if children_map.contains_key("<*>"){ 
                        let next_node = children_map.get("<*>").unwrap();
                        let next_node_clone = Rc::clone(next_node);
                        drop(parent_borrow);
                        parent_node = next_node_clone;
                    }else{
                        return None;
                    }
                }
                _ => unreachable!()
            } 

            current_depth += 1;
        }

        let parent_borrow = parent_node.borrow();
        let log_cluster_l = match &parent_borrow.child_or_logcluster{
            ChildOrLogCluster::LogClusters(log_clusters) => log_clusters,
            _ => unreachable!(),
        };

        let ret_log_clust = self.fast_match(log_cluster_l, seq).cloned();

        ret_log_clust
    }

    fn print_tree(&self, node: &Node, dep: usize){
        let mut p_str = String::new();
        for _ in 0..dep {
            p_str.push('\t');
        }
        
        if node.depth == 0 {
            p_str += "Root";
        } else if node.depth == 1 {
            p_str += &format!("<{}>", node.digit_or_token);
        } else {
            p_str += node.digit_or_token.as_ref();
        }

        println!("{}", p_str);

        if node.depth == self.depth {
            return;
        }

        if let ChildOrLogCluster::Children(children_map) = &node.child_or_logcluster{
            for (_, child) in children_map{
                let child_borrow = child.borrow();
                self.print_tree(&*child_borrow, dep + 1);
            }       
            
        }
    }

    fn output_result(&self, log_clust_l: &Vec<LogCluster>) -> std::result::Result<(), Box<dyn Error>>{
        let mut df_log = match &mut self.df_log {
            Some(df_log) => df_log,
            None => panic!("df_log is None"),
        };
        let row_count = df_log.height();
        let mut log_templates = vec![None;row_count];
        let mut log_templateids = vec![None; row_count];
        let mut df_events = Vec::new();
        // 创建 DataFrame
        // let mut columns: Vec<Series> = Vec::new();
        // let mut series_map: std::collections::HashMap<&String, Vec<String>> = std::collections::HashMap::new();

        // for entry in &log_entries {
        //     for (key, value) in entry {
        //         series_map.entry(key).or_insert_with(Vec::new).push(value.clone());
        //     }
        // }

        // for (key, values) in series_map {
        //     println!("key:{:?}",key);
        //     columns.push(Series::new(key.trim_matches('<').trim_matches('>'), values));
        // }

        // let df = DataFrame::new(columns)?;
        for log_clust in log_clust_l {
            let template_str = log_clust.log_event.join(" ");
            let occurrence = log_clust.log_id_lists.len();
            let template_id = format!("{:x}", Md5::digest(template_str.as_bytes()))[..8].to_string();

            for log_id in &log_clust.log_id_lists {
                let log_id = match log_id.parse::<usize>() {
                    Ok(num) => num,
                    Err(e) => panic!("Failed to parse string: {}", e),
                };
                let idx = log_id - 1;
                log_templates[idx] = Some(template_str);
                log_templateids[idx] = Some(template_id);
            }
            df_events.push(Event {
                event_id: template_id,
                event_template: template_str,
                occurrences: occurrence,
            });
        }

        let log_templateids = Series::new("EventId",log_templateids);
        let log_templates = Series::new("EventTemplate",log_templates);

        let mut parameter_list = Vec::new();
        
        if self.keep_para {
            for row_id in 0..row_count{
                let row = df_log.get_row(row_id);              
                parameter_list.push(self.get_parameter_list(row));
            }
            let mut parameter_series = Series::new("ParameterList", parameter_list);
            let mut df = df_log.with_column(parameter_series)?;
            df_log = df;
        };

        let log_name = match &self.log_name{
            Some(log_name)=>log_name,
            None=> panic!("no log_name"),
        } ;
        // Write structured logs to CSV
        let structured_path = Path::new(&self.out_dir).join(format!("{}_structured.csv", log_name));
        let mut wtr = Writer::from_path(structured_path).expect("Failed to create writer");
        wtr.write_record(["EventId", "EventTemplate", "ParameterList"])
            .expect("Failed to write header");

        for entry in &self.df_log {
            wtr.serialize(entry).expect("Failed to serialize entry");
        }
        wtr.flush().expect("Failed to flush writer");

        // Generate event templates CSV
        let mut occ_dict = HashMap::new();
        for entry in &self.df_log {
            if let Some(template) = &entry.event_template {
                *occ_dict.entry(template.clone()).or_insert(0) += 1;
            }
        }

        let unique_templates: Vec<_> = occ_dict.keys().cloned().collect();
        let mut df_event = Vec::new();
        for template in unique_templates {
            let event_id = format!("{:x}", Md5::digest(template.as_bytes()))[..8].to_string();
            let occurrences = occ_dict[&template];
            df_event.push(Event {
                event_id,
                event_template: template,
                occurrences,
            });
        }

        let templates_path = Path::new(&self.out_dir).join(format!("{}_templates.csv", log_name));
        let mut wtr = Writer::from_path(templates_path).expect("Failed to create writer");
        wtr.write_record(["EventId", "EventTemplate", "Occurrences"])
            .expect("Failed to write header");

        for event in df_event {
            wtr.serialize(event).expect("Failed to serialize event");
        }
        wtr.flush().expect("Failed to flush writer");

        Ok(())
    }

    fn get_parameter_list(&self, row:row::Row<'_>) -> Vec<String> {
        // Step 1: Replace placeholders with <*>
        let template_regex = Regex::new(r"<.{1,5}>").unwrap();
        let template_str = template_regex.replace_all(&self.event_template, "<*>");

        // If there are no placeholders, return an empty list
        if !template_str.contains("<*>") {
            return vec![];
        }

        // Step 2: Escape special characters except for <*>
        let mut escaped_template = String::new();
        for c in template_str.chars() {
            if c.is_alphanumeric() || c == '*' {
                escaped_template.push(c);
            } else {
                escaped_template.push('\\');
                escaped_template.push(c);
            }
        }

        // Step 3: Replace multiple spaces with \s+
        let space_regex = Regex::new(r"\\ +").unwrap();
        let escaped_template = space_regex.replace_all(&escaped_template, r"\\s+");

        // Step 4: Construct the final regex pattern
        let final_pattern = format!("^{}$", escaped_template.replace("<*>", "(.*?)"));

        // Step 5: Find matches in the content
        let re = Regex::new(&final_pattern).unwrap();
        let captures = re.captures(&self.content);

        match captures {
            Some(caps) => caps.iter().skip(1) // Skip the entire match
                .filter_map(|c| c.map(|m| m.as_str().to_string()))
                .collect(),
            None => vec![],
        }
    }
}

#[derive(Debug, Serialize)]
struct Event {
    event_id: String,
    event_template: String,
    occurrences: usize,
}

#[derive(Debug, Serialize)]
struct LogEntry {
    event_id: Option<String>,
    event_template: Option<String>,
    parameter_list: Option<Vec<String>>,
}