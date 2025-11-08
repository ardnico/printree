name: 💬 感想・フィードバック / Feedback
description: プロジェクトへの感想・意見を共有してください / Share your thoughts about this project
title: "✨ [Feedback] "
labels: ["feedback"]
body:
  - type: markdown
    attributes:
      value: |
        ## 💬 感想・フィードバック  
        プロジェクトを使ってみた感想や気づきを気軽に教えてください！  
        Feel free to share your impressions or any thoughts about this project.

  - type: dropdown
    id: rating
    attributes:
      label: ⭐ 総合評価 / Overall Rating
      description: 1 = 不満, 5 = とても満足 / 1 = Poor, 5 = Excellent
      options:
        - ⭐ 1
        - ⭐⭐ 2
        - ⭐⭐⭐ 3
        - ⭐⭐⭐⭐ 4
        - ⭐⭐⭐⭐⭐ 5

  - type: textarea
    id: what-you-liked
    attributes:
      label: 👍 良かった点 / What You Liked
      description: 特に気に入った点があれば教えてください。 / What did you especially like?
      placeholder: 例: ドキュメントがわかりやすかった、使いやすい etc.

  - type: textarea
    id: what-can-improve
    attributes:
      label: 🛠 改善してほしい点 / What Could Be Improved
      description: 改善してほしい点や提案があればぜひ教えてください。 / Any suggestions for improvement?
      placeholder: 例: 〇〇の説明をもう少し詳しく etc.

  - type: textarea
    id: free-comment
    attributes:
      label: 💭 自由コメント / Additional Comments
      description: その他感じたことなど自由にどうぞ。 / Anything else you'd like to share?
      placeholder: 
