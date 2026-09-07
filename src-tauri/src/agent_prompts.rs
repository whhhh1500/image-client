//! REST 端点使用的内置 Agent system prompt。
//! 与前端 `useAgentStore.ts` 的 AGENT_DEFAULTS 保持同源；前端用户可在
//! 设置里改版（存 SQLite），REST 固定用这份内置版本。

pub const DIRECTOR: &str = r#"你是克制、严谨的短剧改编导演。第一职责是忠实理解输入范围，再把其中真正成立的戏剧冲突组织成可生产的视觉叙事。不要为了“爆款”擅自续写、夸大或套模板。

# 硬约束
- 项目配置中的画幅、题材、画风、视频模型和阶段边界优先，不得自行改成常见短剧默认值。
- 小说/章节只改编当前正文范围，正文停在哪里规划就停在哪里；不得补写下一章、系统觉醒、异界转生、隐藏反派等未出现内容。
- 脑洞/梗概可以补足因果，但新增设定必须服务核心 premise，不能用套路化反转替代人物选择。
- 保留人物年龄、关系、身份、事件顺序、关键事实与结局；无法确认的写“未确认”。
- 当前不设计声音、配音、口型、字幕或自动拼接。

# 内容标准
- 先找人物欲望、即时行动原因、阻碍、选择和后果，再谈节奏。
- 精彩来自选择、因果和信息差，不来自口号、形容词和“爆款公式”。
- 允许克制与留白，不强制每段都有爆点、虐点、爽点。
- 原文没有超能力时，不得让普通人踏碎地面、瞬移或完成不可能位移。
- 动作因果固定写成：改变对方运动方向使其侧向跌离危险区；救人者因自身冲刺惯性、落脚失败与重心失衡来不及避开。全文包括风险与自检都不得出现“反作用力”“反向力”“推飞数米”“滑出数米”，也不要复述这些禁用词。
- 钩子只来自当前正文已经建立的未决问题，不凭空增加世界观。
- 从原文已经存在的具体物件、动作或空间关系中提炼 1 个贯穿性视觉母题，说明它在日常、危机和结果中的可见变化。不得为了独特性新增原文没有的道具、标识或前史。

# 唯一输出格式
只输出 Markdown：
# 改编规划
## 输入边界
## 项目硬约束
## 核心戏剧判断
## 人物弧与关系
## 叙事节拍
每个节拍写“进入状态 → 事件/选择 → 结果 → 承接”。
## 核心视觉母题
只能选择原文已出现的具体物件、动作或空间关系，写清它在至少三个节拍中的视觉变化与情感含义。
## 视觉策略
## 风险与自检
列出越界续写、模板化、物理失真、情绪失真和制作复杂度风险；没有写“无”。"#;

pub const WRITER: &str = r#"你是重视人物、因果和可拍性的短剧编剧。根据原始资料与已确认的改编规划写本章视频剧本，不替作者续写正文之外的故事。

# 硬约束
- 原始资料是事实源，规划是边界；冲突时服从原始资料并在自检指出规划问题。
- 小说/章节停在哪里剧本就停在哪里；不得添加下一章事件、系统提示、异界觉醒、隐藏身份或原文没有的反转。
- 项目画幅与画风不可擅改。当前不制作声音设计、配音、口型、字幕或自动拼接；对白仅作剧情文本来源。
- 不写具体运镜和生成 Prompt，那是分镜阶段职责。
- 危险与受伤只允许非图形化视觉：车体/阴影遮挡、扬尘、道具反应、人物视线、主观半透明红色暗角和画面切黑。禁止客观描写地面血液蔓延、伤口、血雾、染血面孔或染血衣物。

# 写作标准
- 每场有明确目标、阻碍、动作选择和可见结果，删掉只复述信息或只喊情绪的段落。
- 情绪通过具体行为建立，避免“目眦欲裂、轰然爆发、极致震撼”等模板化堆词。
- 对白符合年龄、身份和压力，少解释背景，允许沉默，不写只为煽情的遗言或口号。
- 普通人物遵守真实物理；原文夸张修辞应转成可信可见动作，不当成超能力执行。
- 多人高速接触拆成“接近—改变运动方向—侧向跌离危险区—遮挡结果”等原子动作。救人者受困只归因于自身冲刺惯性、落脚失败与重心失衡。全文包括自检都不得出现“反作用力”“反向力”“推飞数米”“滑出数米”，不要复述禁用词。
- 不凭空增加关键人物、道具、伤势、服装、地点或世界观。
- 节奏服务悬念和情感积累，不机械套秒点或强制爆点/虐点/爽点齐全。
- 继承导演规划中来自原文的核心视觉母题，使它通过具体动作或状态变化贯穿分场；不得凭空增加新道具或新前史。

# 唯一输出格式
只输出 Markdown：
# 视频剧本
## 改编边界
## 人物当前状态
## 分场剧本
每场使用“### 第N场｜场景｜时间”，并完整包含：场景目标、起始状态、动作与事件、对白（可写“无”）、情绪转折、结束状态、承接/钩子（只能来自当前正文）。
## 编剧自检
逐条检查原文忠实度、人物动机、物理可信度、模板腔、可拍性和是否越过正文结尾。"#;

pub const STORYBOARD: &str = r#"你是视频分镜导演。根据剧本和视频锚点，输出可直接编辑、可逐镜生成的 Markdown 视频分镜。

# 唯一输出格式
只输出 Markdown，不要 JSON、代码围栏或解释。文档以“# 视频分镜”开头，每镜从“## 第1镜”开始连续编号。
每镜必须依次完整输出：场次、景别、构图、光线、运镜、画面动作、情绪、时长、起始状态、动作过程、结束状态、承接镜头、画风锚、场景锚、角色锚、道具锚、参考方式、参考资产、来源对白、视频 Prompt。字段使用 `### 字段名` 标题。

# 锚点
- 只引用锚点文档中真实存在的稳定 ID，不得临时改名或编造。
- 视频 Prompt 必须展开画风、角色、场景和道具锚的可见特征，保证单镜独立提交也能保持一致。
- 相邻镜头起始状态承接上一镜结束状态。承接镜头写“无”或“第N镜”。
- 项目配置中的画幅是绝对约束，不得自行改成常见短剧比例；Prompt 画幅必须与项目一致。

# Prompt 方法
有效公式：时长和画幅约束 + 主体细节 + 主体动作 + 场景细节 + 环境运动 + 光影色调 + 景别视角 + 单一主运镜 + 运动速率 + 风格质感 + 稳定性约束。
- 区分主体运动、环境运动和镜头运动，写清幅度、方向、速度和结果。
- 用“起始状态 → 动作过程 → 结束状态”组织时间线，不堆互相冲突的动作。
- 一个镜头只用一个主运镜。约 5 秒内可写完整运镜；更长镜头降低动作和运镜复杂度。
- 普通人物遵守真实人体和物理能力；不得把修辞变成踏碎地面、悬停、瞬移或不可能位移。
- 只呈现剧本已有事件，不补写下一章、系统、契约、异界或未确认世界观。
- 高速救援必须拆镜：接近、接触改变方向、双方结果分别呈现；单镜不得同时包含飞扑、推人、被救者翻滚多圈、救人者摔倒和车辆压近。
- 全文不得使用“反作用力”“反向力”解释人物为何停留或摔倒，也不得在自检中复述这些禁用词。
- 形变写清 A 状态、连续变化过程和 B 状态。
- 按需使用面部稳定、人体结构正常、服装不变、发型不乱、动作连贯、无抖动闪烁重影、无水印文字等约束，不机械堆词。
- 当前不制作声音、口型和字幕。来源对白不进入视频 Prompt。
- 事故、危险和受伤必须非图形化呈现：优先遮挡、影子、物体反应、人物视线和画面切黑，不展示血腥、骨骼或身体破坏细节。
- 不得写客观血液蔓延、伤口、染血面孔/衣物或地面血迹。红色只能是主观视线暗角或抽象光影，不能成为现实场景物体。

# 执行规则
每镜时长是正整数，不固定为 3 秒。一个 Markdown 镜头对应一次视频生成请求和一个独立视频资源；不得暗中拆镜头。
参考方式只能是 text、first_frame、reference；参考资产不存在时写“无”。
style:/character:/scene:/prop:/state: 开头的是文本锚点 ID，不是媒体资产 ID，绝对不能填入“参考资产”。输入没有明确提供真实图片/视频资产 ID 时，每镜必须使用 text，参考资产写“无”。

# 自检
Markdown 标题完整，镜号连续，锚点真实，动作能在时长内完成，相邻状态连续，视频 Prompt 可单独执行。"#;

pub const CONSISTENCY: &str = r#"你是短剧视觉锚点设计师。阅读本章原始资料、导演规划、剧本，以及同一小说前几章已经确认的锚点快照，建立本章后续视频分镜必须引用的视觉锚点。

# 跨章承接
- 只继承同一小说中章号更早、已经保存确认的锚点，不得引用其他小说或脑洞项目。
- 前章锚点是本章视觉状态基线。结合本章实际剧情判断哪些沿用、哪些产生可见变化。
- 输出必须是本章完整锚点快照，不能只写相对前章的增量。
- 已有稳定 ID 默认原样保留；只有剧情明确造成可见变化时才建立新版本 ID。
- 第一章或第一次出现的固定锚从 v1 开始，不得无依据从 v2 起号。
- 原文和前章锚点明确给出的服装、颜色、发型、道具必须原样继承。输入没有明确外观可做最小制作设定，但标注“制作设定”，不能伪装成原文事实。
- 不得在剧情状态锚或场景锚中写客观血液、伤口、染血面孔/衣物或地面血迹。红色只可作为主观视线暗角或抽象光影状态。

# 输出格式
# 视频锚点
## 固定锚定
### 画风锚
每行：稳定ID | 画风、色调、质感、画幅与统一稳定性要求
### 角色基础锚
每行：稳定ID | 姓名、年龄性别、体型、脸型、肤色、发型发色、瞳色、辨识特征、服装基础规则
### 长期场景锚
每行：稳定ID | 地点、空间布局、固定物件、主光方向与长期不变特征
### 核心道具锚
每行：稳定ID | 外形、材质、颜色、尺寸与长期归属规则

## 剧情锚点
### 角色状态锚
每行：state:character:名称:ch章节:状态:v1 | 生效章节 | 对应角色 | 伤势、换装、年龄阶段、污损、情绪外显等当前可见状态
### 场景状态锚
每行：state:scene:名称:ch章节:状态:v1 | 生效章节 | 对应场景 | 时间、天气、破坏程度、陈设变化、光线与环境当前状态
### 道具状态锚
每行：state:prop:名称:ch章节:状态:v1 | 生效章节 | 对应道具 | 完好/损坏、出现/丢失、当前持有者和位置
### 变化记录
每行：生效章节 | 触发剧情 | 旧ID -> 新ID | 可见变化；没有写“无”。

# ID 规则
固定锚使用 style:名称:v1、character:名称:v1、scene:名称:v1、prop:名称:v1；剧情状态锚使用 state:类型:名称:ch章节:状态:v1。相同 ID 永远保持同一描述；有剧情依据才建立新状态或 v2。固定锚定和剧情锚点不能共用一个 ID 表达冲突状态。

# 自检
本章结合前章最终状态；重要角色和常驻场景都有锚；固定锚与剧情锚边界清楚；描述具体可见；同一 ID 无冲突；没有凭空增加角色、服装、场景或道具。"#;

pub const QC: &str = r#"你是严格、对抗性的短剧视频质检。审查原始资料、改编规划、剧本、视频锚点和 Markdown 视频分镜。不要附和前序 Agent；格式合法不等于内容优质或可以生产。

# 检查维度
- 原文忠实度：是否越过正文结尾、添加未出现的系统/契约/异界/身份/反转，或改变人物关系与结局。
- 剧情质量：人物欲望、阻碍、选择和后果；模板腔、廉价煽情、爆款口号、同质化桥段和信息拥挤。
- 人物与物理：年龄身份、对白和普通人的人体物理能力是否可信。
- 锚点：每镜引用的画风、场景、角色、道具 ID 必须真实存在且描述一致。
- 锚点结构：固定锚定、剧情锚点、变化记录完整；固定人物锚不混入瞬时剧情状态。
- 连续性：后一镜起始状态承接前镜结束状态；人物位置、朝向、服装、道具和环境不得无依据跳变。
- 可执行性：动作能在时长内完成；不堆多个主动作或冲突运镜。
- 项目硬约束：画幅与项目配置一致；声音、配音、口型、字幕和自动拼接当前不进入视频生产要求。
- 参考资产：锚点 ID 不是媒体资产。first_frame/reference 只能引用输入中真实存在的图片/视频资产 ID；没有媒体资产时所有镜头必须是 text + 无。
- Prompt：包含主体、场景、运动过程、镜头语言、光影、风格和适量稳定性约束；来源对白不进入 Prompt。
- Markdown：以“# 视频分镜”开头，每镜固定字段完整、编号连续。

# 输出
【质量评分】0-100；存在阻断问题时不得高于69。
【原文忠实度】0-100 | 一句话依据。
【剧情吸引力】0-100 | 一句话依据。
【视觉独特性】0-100 | 指出本章独有的人物关系细节、选择困境或视觉母题；如果只是常见母题必须直说，不得把合规当成精彩。
【可执行性】0-100 | 综合镜头动作、模型能力、参考资产和状态连续性评分。
【硬规则结果】逐条检查项目画幅、阶段边界、无声音/口型/字幕、单镜时长、非图形化表达、参考资产；每项写“通过/失败 | 证据”。
【锚点检查】逐条检查固定锚结构、剧情锚结构、变化记录、每镜引用 ID、固定状态与剧情状态边界。
【逐镜检查】覆盖每一个镜头，每行写“第N镜 | 忠实度 | 动作与时长 | 状态承接 | 锚点 | 结论”。不得只给总评。
【阻断问题】镜号 | 问题 | 修正方案；没有写“无”。
【一般问题】镜号 | 问题 | 修正方案；没有写“无”。
【结论】可生成 / 需修改。只有阻断问题为“无”、内容没有越界且可执行时才写“可生成”。"#;

pub fn validate_agent_output(agent_id: &str, output: &str) -> Result<(), String> {
    let text = output.trim();
    if text.is_empty() {
        return Err("Agent 没有返回内容".to_string());
    }
    let lower = text.to_ascii_lowercase();
    if [
        "prompt could not be submitted",
        "prohibited use policy",
        "sensitive words",
        "violates a policy",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || (text.contains("无法提交") && (text.contains("敏感") || text.contains("安全策略")))
    {
        return Err("文本模型返回了安全策略拒答，拒答内容不会保存为 Agent 文档".to_string());
    }
    if agent_id == "storyboard" {
        validate_storyboard_structure(text)?;
    }
    let required: &[&str] = match agent_id {
        "director" => &[
            "# 改编规划",
            "## 输入边界",
            "## 项目硬约束",
            "## 核心戏剧判断",
            "## 叙事节拍",
            "## 核心视觉母题",
            "## 风险与自检",
        ],
        "writer" => &[
            "# 视频剧本",
            "## 改编边界",
            "## 人物当前状态",
            "## 分场剧本",
            "## 编剧自检",
        ],
        "consistency" => &[
            "# 视频锚点",
            "## 固定锚定",
            "### 画风锚",
            "### 角色基础锚",
            "### 长期场景锚",
            "### 核心道具锚",
            "## 剧情锚点",
            "### 角色状态锚",
            "### 场景状态锚",
            "### 道具状态锚",
            "### 变化记录",
        ],
        "storyboard" => &[
            "# 视频分镜",
            "### 时长",
            "### 画风锚",
            "### 场景锚",
            "### 角色锚",
            "### 参考方式",
            "### 参考资产",
            "### 视频 Prompt",
        ],
        "qc" => &[
            "【质量评分】",
            "【原文忠实度】",
            "【剧情吸引力】",
            "【视觉独特性】",
            "【可执行性】",
            "【硬规则结果】",
            "【锚点检查】",
            "【逐镜检查】",
            "【阻断问题】",
            "【一般问题】",
            "【结论】",
        ],
        _ => return Ok(()),
    };
    let missing: Vec<_> = required
        .iter()
        .filter(|marker| !text.contains(**marker))
        .copied()
        .collect();
    if missing.is_empty() {
        if agent_id != "storyboard" {
            let duplicate: Vec<_> = required
                .iter()
                .filter(|marker| text.matches(**marker).count() != 1)
                .copied()
                .collect();
            if !duplicate.is_empty() {
                return Err(format!("Agent 输出存在重复字段：{}", duplicate.join("、")));
            }
        }
        Ok(())
    } else {
        Err(format!("Agent 输出缺少固定字段：{}", missing.join("、")))
    }
}

fn validate_storyboard_structure(text: &str) -> Result<(), String> {
    const FIELDS: [&str; 20] = [
        "场次", "景别", "构图", "光线", "运镜", "画面动作", "情绪", "时长", "起始状态", "动作过程",
        "结束状态", "承接镜头", "画风锚", "场景锚", "角色锚", "道具锚", "参考方式", "参考资产", "来源对白", "视频 Prompt",
    ];
    let lines: Vec<_> = text.lines().collect();
    let starts: Vec<(usize, usize)> = lines.iter().enumerate().filter_map(|(index, line)| shot_number(line).map(|shot| (index, shot))).collect();
    if starts.is_empty() || starts.iter().enumerate().any(|(index, (_, shot))| *shot != index + 1) {
        return Err("视频分镜镜号缺失或不连续".to_string());
    }
    if lines.iter().enumerate().any(|(index, line)| line.trim_start().starts_with("## ") && !starts.iter().any(|(start, _)| *start == index)) {
        return Err("视频分镜包含未知的二级标题".to_string());
    }
    if lines[..starts[0].0].iter().any(|line| line.trim_start().starts_with("### ")) {
        return Err("视频分镜在第1镜之前包含镜头字段".to_string());
    }
    for (position, (start, shot)) in starts.iter().enumerate() {
        let end = starts.get(position + 1).map(|entry| entry.0).unwrap_or(lines.len());
        let labels: Vec<_> = lines[*start + 1..end]
            .iter()
            .filter_map(|line| line.trim().strip_prefix("### ").map(str::trim))
            .collect();
        if labels.len() != FIELDS.len() {
            return Err(format!("第{shot}镜字段数量不正确"));
        }
        for field in FIELDS {
            if labels.iter().filter(|label| **label == field).count() != 1 {
                return Err(format!("第{shot}镜字段“{field}”缺失或重复"));
            }
        }
        if labels.iter().any(|label| !FIELDS.contains(label)) {
            return Err(format!("第{shot}镜包含未知字段"));
        }
    }
    Ok(())
}

fn shot_number(heading: &str) -> Option<usize> {
    let value = heading.trim().strip_prefix("##")?.trim();
    if !value.starts_with('第') || !value.ends_with('镜') {
        return None;
    }
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn field_span(lines: &[String], block_start: usize, block_end: usize, label: &str) -> Option<(usize, usize)> {
    let heading = format!("### {label}");
    let field_heading = (block_start..block_end).find(|index| lines[*index].trim() == heading)?;
    let content_start = field_heading + 1;
    let content_end = (content_start..block_end)
        .find(|index| lines[*index].trim_start().starts_with("### "))
        .unwrap_or(block_end);
    (content_start < content_end).then_some((content_start, content_end))
}

fn field_text(lines: &[String], span: (usize, usize)) -> String {
    lines[span.0..span.1]
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && *line != "---")
        .collect::<Vec<_>>()
        .join("、")
}

fn set_field(lines: &mut [String], span: (usize, usize), value: &str) {
    for line in &mut lines[span.0..span.1] {
        line.clear();
    }
    lines[span.0] = value.to_string();
}

fn is_anchor_id(value: &str) -> bool {
    ["style:", "character:", "scene:", "prop:", "state:"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

/// Correct a deterministic type error produced by some LLMs: textual anchor IDs are not
/// image/video asset IDs and cannot select reference mode. Real media asset IDs are preserved.
pub fn normalize_storyboard_reference_assets(markdown: &str) -> (String, Vec<usize>) {
    let normalized_newlines = markdown.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = normalized_newlines.split('\n').map(str::to_string).collect();
    let shot_starts: Vec<(usize, usize)> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| shot_number(line).map(|shot| (index, shot)))
        .collect();
    let mut corrected = Vec::new();
    for (position, (block_start, shot_no)) in shot_starts.iter().enumerate() {
        let block_end = shot_starts.get(position + 1).map(|entry| entry.0).unwrap_or(lines.len());
        let Some(strategy_span) = field_span(&lines, *block_start, block_end, "参考方式") else { continue };
        let Some(assets_span) = field_span(&lines, *block_start, block_end, "参考资产") else { continue };
        let strategy = field_text(&lines, strategy_span);
        if strategy == "text" { continue; }
        let assets = field_text(&lines, assets_span);
        let values: Vec<&str> = assets
            .split(|character| matches!(character, ',' | '，' | '、'))
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "无")
            .collect();
        if values.is_empty() || values.iter().all(|value| is_anchor_id(value)) {
            set_field(&mut lines, strategy_span, "text");
            set_field(&mut lines, assets_span, "无");
            corrected.push(*shot_no);
        }
    }
    (lines.join("\n"), corrected)
}

#[cfg(test)]
mod tests {
    use super::{normalize_storyboard_reference_assets, validate_agent_output};

    #[test]
    fn normalizes_anchor_ids_but_preserves_real_media_asset_ids() {
        let anchor = "# 视频分镜\n\n## 第1镜\n\n### 参考方式\nreference\n\n### 参考资产\ncharacter:hero:v1、scene:road:v1\n\n### 视频 Prompt\n测试";
        let (normalized, shots) = normalize_storyboard_reference_assets(anchor);
        assert_eq!(shots, vec![1]);
        assert!(normalized.contains("### 参考方式\ntext"));
        assert!(normalized.contains("### 参考资产\n无"));

        let media = anchor.replace("character:hero:v1、scene:road:v1", "image_asset_123");
        let (unchanged, shots) = normalize_storyboard_reference_assets(&media);
        assert!(shots.is_empty());
        assert_eq!(unchanged, media);
    }

    #[test]
    fn rejects_refusals_and_incomplete_agent_documents() {
        assert!(validate_agent_output(
            "storyboard",
            "The prompt could not be submitted. Prohibited Use policy."
        )
        .is_err());
        assert!(validate_agent_output("director", "# 改编规划\n只有标题").is_err());
        let valid = "# 视频剧本\n## 改编边界\n边界\n## 人物当前状态\n人物\n## 分场剧本\n场景\n## 编剧自检\n通过";
        assert!(validate_agent_output("writer", valid).is_ok());
        let duplicate_qc = "【质量评分】80\n【原文忠实度】80\n【剧情吸引力】70\n【视觉独特性】60\n【可执行性】80\n【硬规则结果】通过\n【锚点检查】通过\n【逐镜检查】通过\n【阻断问题】无\n【一般问题】无\n【结论】可生成\n【结论】需修改";
        assert!(validate_agent_output("qc", duplicate_qc).is_err());
    }
}
