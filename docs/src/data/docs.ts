// Bootstrap edits only this identity block; prose pages remain project-owned.
const generatedIdentity = {
  "name": "boatparty",
  "strapline": "Warm boat.dev sandbox pools for any Git project",
  "description": "Warm boat.dev sandbox pools for any Git project",
  "repoUrl": "https://github.com/amxv/boatparty",
  "footerText": "Warm boat.dev sandbox pools for any Git project",
  "distributionText": "GitHub Releases for macOS and Linux, installed with gh."
} as const;

export const siteConfig = {
  name: generatedIdentity.name,
  strapline: generatedIdentity.strapline,
  description: generatedIdentity.description,
  repoUrl: generatedIdentity.repoUrl,
  logoHref: "/favicon.svg",
  accentColor: "#b7410e",
  accentColorDark: "#f28c52",
  footerSections: [
    {
      title: generatedIdentity.name,
      text: generatedIdentity.footerText
    },
    {
      title: "Distribution",
      text: generatedIdentity.distributionText
    },
    {
      title: "Repository",
      linkPrefix: "Source: ",
      linkHref: generatedIdentity.repoUrl,
      linkLabel: generatedIdentity.repoUrl.replace("https://", "")
    }
  ]
} as const;

export const docCategories = ["Start", "Guides", "Reference", "Distribution"] as const;

export const primaryNav = [
  { href: "/docs", label: "Docs" },
  { href: siteConfig.repoUrl, label: "GitHub", external: true }
];
