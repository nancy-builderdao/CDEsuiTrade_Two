import matplotlib.pyplot as plt
import pandas as pd
import seaborn as sns
import re

# ==========================================
# 1. 請將你的 100 筆數據完整貼在下面的引號內
# ==========================================
raw_data = """
1   | 1142 ms | 6  blocks | -0.2277%
2   | 885  ms | 6  blocks | -0.2277%
3   | 1122 ms | 5  blocks | -0.2277%
4   | 1027 ms | 6  blocks | -0.2277%
5   | 1095 ms | 6  blocks | -0.2277%
6   | 1126 ms | 6  blocks | -0.2277%
7   | 999  ms | 6  blocks | -0.2277%
8   | 1684 ms | 5  blocks | -0.2277%
9   | 1084 ms | 8  blocks | -0.2277%
10  | 1062 ms | 5  blocks | -0.2277%
"""
# ... (這裡繼續貼上你其他的數據) ...

# ==========================================
# 2. 數據解析邏輯 (Regex)
# ==========================================
data = []
pattern = r"(\d+)\s+\|\s+(\d+)\s+ms\s+\|\s+(\d+)\s+blocks\s+\|\s+([-\d.]+)%"

for line in raw_data.strip().split('\n'):
    match = re.search(pattern, line)
    if match:
        data.append({
            "Round": int(match.group(1)),
            "Latency": int(match.group(2)),
            "Lag": int(match.group(3)),
            "Diff": float(match.group(4))
        })

df = pd.DataFrame(data)

# 顯示基本統計數據
print("📊 統計數據摘要：")
print(df.describe())

# ==========================================
# 3. 繪製分佈圖
# ==========================================
# 設定風格
sns.set(style="whitegrid")
plt.figure(figsize=(20, 6))

# --- 圖表 1: Latency 分佈 (直方圖 + 密度曲線) ---
plt.subplot(1, 3, 1)
sns.histplot(data=df, x="Latency", kde=True, color="skyblue", bins=15)
plt.title(f"Latency Distribution (Avg: {df['Latency'].mean():.1f} ms)")
plt.xlabel("Latency (ms)")
plt.ylabel("Frequency")

# --- 圖表 2: Lag 分佈 (長條圖) ---
plt.subplot(1, 3, 2)
# ✨ 修正點：改用 color="salmon" 避免 KeyError
sns.countplot(data=df, x="Lag", color="salmon")
plt.title(f"Checkpoint Lag Distribution (Avg: {df['Lag'].mean():.1f})")
plt.xlabel("Lag (Blocks)")
plt.ylabel("Count")

# --- 圖表 3: Price Diff 分佈 (直方圖) ---
plt.subplot(1, 3, 3)
sns.histplot(data=df, x="Diff", kde=True, color="lightgreen", bins=10)
plt.title(f"Price Diff% Distribution (Avg: {df['Diff'].mean():.4f}%)")
plt.xlabel("Price Diff (%)")
plt.ylabel("Frequency")

plt.tight_layout()
plt.show()