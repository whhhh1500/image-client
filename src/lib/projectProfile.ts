import type { Project } from "../store/useProjectStore";
import { useGenerationStore } from "../store/useGenerationStore";
import { useVideoStore } from "../store/useVideoStore";

export const STORY_STYLES = ["通用短剧", "都市情感", "悬疑惊悚", "喜剧", "热血动作", "古装仙侠", "科幻末世"];
export const ART_STYLES = ["电影写实", "真人现代都市", "真人古装", "2D 国风", "2D 日漫", "3D 动画渲染", "3D 国风"];
export const ASPECT_RATIOS = ["16:9", "9:16", "1:1", "3:2", "2:3"];

export function imageSizeForRatio(ratio: string): string {
  return ({
    "16:9": "1280x720 (16:9)",
    "9:16": "720x1280 (9:16)",
    "3:2": "1536x1024 (3:2)",
    "2:3": "1024x1536 (2:3)",
  } as Record<string, string>)[ratio] ?? "1024x1024 (1:1)";
}

export function applyProjectProfile(project: Project) {
  useGenerationStore.getState().set({
    model: project.imageModel,
    quality: project.imageQuality,
    size: imageSizeForRatio(project.aspectRatio),
  });
  useVideoStore.getState().set({
    model: project.videoModel,
    aspectRatio: project.aspectRatio,
    resolution: project.videoResolution || "720p",
  });
}

export function projectContext(project: Project): string {
  const lines = [
    `项目：${project.name}`,
    project.description ? `项目简介：${project.description}` : "",
    `题材：${project.storyStyle}`,
    `画风：${project.artStyle}`,
    `目标画幅：${project.aspectRatio}`,
  ].filter(Boolean);
  return `【项目生产档案】\n${lines.join("\n")}`;
}
